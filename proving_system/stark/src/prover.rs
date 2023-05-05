use super::{
    air::{constraints::evaluator::ConstraintEvaluator, frame::Frame, trace::TraceTable, AIR},
    fri::fri_commit_phase,
    sample_z_ood,
};
use crate::{
    batch_sample_challenges,
    fri::{
        fri_commitment::FriLayer,
        fri_decommit::{open_layer, FriDecommitment},
        HASHER,
    },
    proof::{DeepConsistencyCheck, StarkProof},
    transcript_to_field, transcript_to_usize, Domain,
};
#[cfg(not(feature = "test_fiat_shamir"))]
use lambdaworks_crypto::fiat_shamir::default_transcript::DefaultTranscript;
use lambdaworks_crypto::{fiat_shamir::transcript::Transcript, merkle_tree::merkle::MerkleTree};

#[cfg(feature = "test_fiat_shamir")]
use lambdaworks_crypto::fiat_shamir::test_transcript::TestTranscript;

use lambdaworks_fft::{errors::FFTError, polynomial::FFTPoly};
use lambdaworks_math::{
    field::{element::FieldElement, traits::IsFFTField},
    polynomial::Polynomial,
    traits::ByteConversion,
};
use log::info;

struct Round1<F: IsFFTField> {
    trace_polys: Vec<Polynomial<FieldElement<F>>>,
    lde_trace: TraceTable<F>,
    lde_trace_merkle_trees: Vec<MerkleTree<F>>,
    lde_trace_merkle_roots: Vec<FieldElement<F>>,
}

struct Round2<F: IsFFTField> {
    composition_poly_even: Polynomial<FieldElement<F>>,
    lde_composition_poly_even_evaluations: Vec<FieldElement<F>>,
    composition_poly_odd: Polynomial<FieldElement<F>>,
    lde_composition_poly_odd_evaluations: Vec<FieldElement<F>>,
    // Merkle trees of H_1 and H_2 at the LDE Domain
    composition_poly_merkle_trees: Vec<MerkleTree<F>>,
    // Commitments of H_1, and H_2
    composition_poly_roots: Vec<FieldElement<F>>,
}

struct Round3<F: IsFFTField> {
    trace_ood_frame_evaluations: Frame<F>,
    composition_poly_ood_evaluations: [FieldElement<F>; 2],
}

struct Round4<F: IsFFTField> {
    fri_last_value: FieldElement<F>,
    fri_layers_merkle_roots: Vec<FieldElement<F>>,
    deep_consistency_check: DeepConsistencyCheck<F>,
    query_list: Vec<FriDecommitment<F>>,
}

#[cfg(feature = "test_fiat_shamir")]
fn round_0_transcript_initialization() -> TestTranscript {
    TestTranscript::new()
}

#[cfg(not(feature = "test_fiat_shamir"))]
fn round_0_transcript_initialization() -> DefaultTranscript {
    // TODO: add strong fiat shamir
    DefaultTranscript::new()
}

fn batch_commit<F>(
    vectors: Vec<&Vec<FieldElement<F>>>,
) -> (Vec<MerkleTree<F>>, Vec<FieldElement<F>>)
where
    F: IsFFTField,
    FieldElement<F>: ByteConversion,
{
    let trees: Vec<_> = vectors
        .iter()
        .map(|col| MerkleTree::build(col, Box::new(HASHER)))
        .collect();

    let roots = trees.iter().map(|tree| tree.root.clone()).collect();
    (trees, roots)
}

fn evaluate_polynomial_on_lde_domain<F>(
    p: &Polynomial<FieldElement<F>>,
    domain: &Domain<F>,
) -> Result<Vec<FieldElement<F>>, FFTError>
where
    F: IsFFTField,
    Polynomial<FieldElement<F>>: FFTPoly<F>,
{
    // Evaluate those polynomials t_j on the large domain D_LDE.
    p.evaluate_offset_fft(
        domain.blowup_factor,
        Some(domain.interpolation_domain_size),
        &domain.coset_offset,
    )
}

fn commit_original_trace<F>(trace: &TraceTable<F>, domain: &Domain<F>) -> Round1<F>
where
    F: IsFFTField,
    FieldElement<F>: ByteConversion,
{
    // The trace M_ij is part of the input. Interpolate the polynomials t_j
    // corresponding to the first part of the RAP.
    let trace_polys = trace.compute_trace_polys();

    // Evaluate those polynomials t_j on the large domain D_LDE.
    let lde_trace_evaluations = trace_polys
        .iter()
        .map(|poly| evaluate_polynomial_on_lde_domain(poly, &domain))
        .collect::<Result<Vec<Vec<FieldElement<F>>>, FFTError>>()
        .unwrap();

    let lde_trace = TraceTable::new_from_cols(&lde_trace_evaluations);

    // Compute commitments [t_j].
    let (lde_trace_merkle_trees, lde_trace_merkle_roots) =
        batch_commit(lde_trace.cols().iter().collect());

    Round1 {
        trace_polys,
        lde_trace,
        lde_trace_merkle_trees,
        lde_trace_merkle_roots,
    }
}

fn commit_extended_trace() {
    // TODO
}

fn round_1_randomized_air_with_preprocessing<F: IsFFTField>(
    trace: &TraceTable<F>,
    domain: &Domain<F>,
) -> Round1<F>
where
    FieldElement<F>: ByteConversion,
{
    let round_1_result = commit_original_trace(trace, &domain);
    commit_extended_trace();
    round_1_result
}

fn round_2_compute_composition_polynomial<F, A>(
    air: &A,
    domain: &Domain<F>,
    round_1_result: &Round1<F>,
    transition_coeffs: &[(FieldElement<F>, FieldElement<F>)],
    boundary_coeffs: &[(FieldElement<F>, FieldElement<F>)],
) -> Round2<F>
where
    F: IsFFTField,
    A: AIR<Field = F>,
    FieldElement<F>: ByteConversion,
{
    // Create evaluation table
    let evaluator = ConstraintEvaluator::new(
        air,
        &round_1_result.trace_polys,
        &domain.trace_primitive_root,
    );

    let constraint_evaluations = evaluator.evaluate(
        &round_1_result.lde_trace,
        &domain.lde_roots_of_unity_coset,
        transition_coeffs,
        boundary_coeffs,
    );

    // Get the composition poly H
    let composition_poly =
        constraint_evaluations.compute_composition_poly(&domain.lde_roots_of_unity_coset);

    let (composition_poly_even, composition_poly_odd) = composition_poly.even_odd_decomposition();

    let lde_composition_poly_even_evaluations =
        evaluate_polynomial_on_lde_domain(&composition_poly_even, &domain).unwrap();
    let lde_composition_poly_odd_evaluations =
        evaluate_polynomial_on_lde_domain(&composition_poly_odd, &domain).unwrap();

    let (composition_poly_merkle_trees, composition_poly_roots) = batch_commit(vec![
        &lde_composition_poly_even_evaluations,
        &lde_composition_poly_odd_evaluations,
    ]);

    Round2 {
        composition_poly_even,
        lde_composition_poly_even_evaluations,
        composition_poly_odd,
        lde_composition_poly_odd_evaluations,
        composition_poly_merkle_trees,
        composition_poly_roots,
    }
}

fn round_3_evaluate_polynomials_in_out_of_domain_element<F: IsFFTField, A: AIR<Field = F>>(
    air: &A,
    domain: &Domain<F>,
    round_1_result: &Round1<F>,
    round_2_result: &Round2<F>,
    z: &FieldElement<F>,
) -> Round3<F>
where
    FieldElement<F>: ByteConversion,
{
    let z_squared = z * z;

    // Evaluate H_1 and H_2 in z^2.
    let composition_poly_ood_evaluations = [
        round_2_result.composition_poly_even.evaluate(&z_squared),
        round_2_result.composition_poly_odd.evaluate(&z_squared),
    ];

    // Returns the Out of Domain Frame for the given trace polynomials, out of domain evaluation point (called `z` in the literature),
    // frame offsets given by the AIR and primitive root used for interpolating the trace polynomials.
    // An out of domain frame is nothing more than the evaluation of the trace polynomials in the points required by the
    // verifier to check the consistency between the trace and the composition polynomial.
    //
    // In the fibonacci example, the ood frame is simply the evaluations `[t(z), t(z * g), t(z * g^2)]`, where `t` is the trace
    // polynomial and `g` is the primitive root of unity used when interpolating `t`.
    let ood_trace_evaluations = Frame::get_trace_evaluations(
        &round_1_result.trace_polys,
        z,
        &air.context().transition_offsets,
        &domain.trace_primitive_root,
    );

    let trace_ood_frame_data = ood_trace_evaluations.into_iter().flatten().collect();
    let trace_ood_frame_evaluations =
        Frame::new(trace_ood_frame_data, round_1_result.trace_polys.len());

    Round3 {
        trace_ood_frame_evaluations,
        composition_poly_ood_evaluations,
    }
}

#[allow(clippy::too_many_arguments)]
fn fri_query_phase<F: IsFFTField, A: AIR<Field = F>, T: Transcript>(
    air: &A,
    domain: &Domain<F>,
    fri_layers: &Vec<FriLayer<F>>,
    transcript: &mut T,
) -> (Vec<FriDecommitment<F>>, usize)
where
    FieldElement<F>: ByteConversion,
{
    if let Some(fri_first_layer) = fri_layers.get(0) {
        let number_of_queries = air.context().options.fri_number_of_queries;
        let mut iotas: Vec<usize> = Vec::with_capacity(number_of_queries);
        let query_list = (0..number_of_queries)
            .map(|_| {
                let iota_s = transcript_to_usize(transcript) % 2_usize.pow(domain.lde_root_order);
                let (first_layer_evaluation, first_layer_auth_path) =
                    open_layer(fri_first_layer, iota_s);

                let mut layers_auth_paths_sym = vec![];
                let mut layers_evaluations_sym = vec![];

                for layer in fri_layers {
                    // symmetric element
                    let index_sym = (iota_s + layer.domain.len() / 2) % layer.domain.len();
                    let (evaluation_sym, auth_path_sym) = open_layer(layer, index_sym);

                    layers_auth_paths_sym.push(auth_path_sym);
                    layers_evaluations_sym.push(evaluation_sym);
                }
                iotas.push(iota_s);

                FriDecommitment {
                    layers_auth_paths_sym,
                    layers_evaluations_sym,
                    first_layer_evaluation,
                    first_layer_auth_path,
                }
            })
            .collect();

        (query_list, iotas[0])
    } else {
        (vec![], 0)
    }
}

fn round_4_compute_and_run_fri_on_the_deep_composition_polynomial<
    F: IsFFTField,
    A: AIR<Field = F>,
    T: Transcript,
>(
    air: &A,
    domain: &Domain<F>,
    round_1_result: &Round1<F>,
    round_2_result: &Round2<F>,
    z: &FieldElement<F>,
    transcript: &mut T,
) -> Round4<F>
where
    FieldElement<F>: ByteConversion,
{
    let trace_poly_coeffients = batch_sample_challenges::<F, T>(
        air.context().transition_offsets.len() * air.context().trace_columns,
        transcript,
    );

    let composition_poly_coeffients = [
        transcript_to_field(transcript),
        transcript_to_field(transcript),
    ];

    // Compute DEEP composition polynomial so we can commit to it using FRI.
    let deep_composition_poly = compute_deep_composition_poly(
        air,
        &round_1_result.trace_polys,
        &round_2_result.composition_poly_even,
        &round_2_result.composition_poly_odd,
        z,
        &domain.trace_primitive_root,
        &composition_poly_coeffients,
        &trace_poly_coeffients,
    );

    // * Do FRI on the deep composition polynomial
    let (fri_last_value, fri_layers) = fri_commit_phase(
        domain.root_order as usize,
        deep_composition_poly,
        &domain.lde_roots_of_unity_coset,
        transcript,
    );

    let fri_layers_merkle_roots: Vec<_> = fri_layers
        .iter()
        .map(|layer| layer.merkle_tree.root.clone())
        .collect();

    let (query_list, iota_0) = fri_query_phase(air, domain, &fri_layers, transcript);

    let deep_consistency_check =
        build_deep_consistency_check(domain, round_1_result, round_2_result, iota_0);

    Round4 {
        fri_last_value,
        fri_layers_merkle_roots,
        deep_consistency_check,
        query_list,
    }
}

/// Returns the DEEP composition polynomial that the prover then commits to using
/// FRI. This polynomial is a linear combination of the trace polynomial and the
/// composition polynomial, with coefficients sampled by the verifier (i.e. using Fiat-Shamir).
fn compute_deep_composition_poly<A: AIR, F: IsFFTField>(
    air: &A,
    trace_polys: &[Polynomial<FieldElement<F>>],
    even_composition_poly: &Polynomial<FieldElement<F>>,
    odd_composition_poly: &Polynomial<FieldElement<F>>,
    ood_evaluation_point: &FieldElement<F>,
    primitive_root: &FieldElement<F>,
    composition_poly_coeffients: &[FieldElement<F>; 2],
    trace_poly_coeffients: &[FieldElement<F>],
) -> Polynomial<FieldElement<F>> {
    let transition_offsets = air.context().transition_offsets;

    // Get trace evaluations needed for the trace terms of the deep composition polynomial
    let trace_evaluations = Frame::get_trace_evaluations(
        trace_polys,
        ood_evaluation_point,
        &transition_offsets,
        primitive_root,
    );

    // Compute all the trace terms of the deep composition polynomial. There will be one
    // term for every trace polynomial and every trace evaluation.
    let mut trace_terms = Polynomial::zero();
    for (i, trace_poly) in trace_polys.iter().enumerate() {
        for (j, (trace_evaluation, offset)) in trace_evaluations
            .iter()
            .zip(&transition_offsets)
            .enumerate()
        {
            let eval = trace_evaluation[i].clone();
            let shifted_root_of_unity = ood_evaluation_point * primitive_root.pow(*offset);
            let poly = (trace_poly - eval)
                / Polynomial::new(&[-shifted_root_of_unity, FieldElement::one()]);

            trace_terms =
                trace_terms + poly * &trace_poly_coeffients[i * trace_evaluations.len() + j];
        }
    }

    let ood_point_squared = ood_evaluation_point * ood_evaluation_point;

    let even_composition_poly_term = (even_composition_poly.clone()
        - Polynomial::new_monomial(even_composition_poly.evaluate(&ood_point_squared), 0))
        / (Polynomial::new_monomial(FieldElement::one(), 1)
            - Polynomial::new_monomial(ood_point_squared.clone(), 0));

    let odd_composition_poly_term = (odd_composition_poly.clone()
        - Polynomial::new_monomial(odd_composition_poly.evaluate(&ood_point_squared), 0))
        / (Polynomial::new_monomial(FieldElement::one(), 1)
            - Polynomial::new_monomial(ood_point_squared.clone(), 0));

    trace_terms
        + even_composition_poly_term * &composition_poly_coeffients[0]
        + odd_composition_poly_term * &composition_poly_coeffients[1]
}

fn build_deep_consistency_check<F: IsFFTField>(
    domain: &Domain<F>,
    round_1_result: &Round1<F>,
    round_2_result: &Round2<F>,
    index_to_open: usize,
) -> DeepConsistencyCheck<F>
where
    FieldElement<F>: ByteConversion,
{
    // Trace openings
    let index = index_to_open % domain.lde_roots_of_unity_coset.len();
    let lde_trace_merkle_proofs = round_1_result
        .lde_trace_merkle_trees
        .iter()
        .map(|tree| tree.get_proof_by_pos(index).unwrap())
        .collect();
    let lde_trace_evaluations = round_1_result.lde_trace.get_row(index).to_vec();

    // Composition polynomial openings

    let lde_composition_poly_proofs = round_2_result
        .composition_poly_merkle_trees
        .iter()
        .map(|tree| tree.get_proof_by_pos(index).unwrap())
        .collect();
    let lde_composition_poly_evaluations = vec![
        round_2_result.lde_composition_poly_even_evaluations[index].clone(),
        round_2_result.lde_composition_poly_odd_evaluations[index].clone(),
    ];

    DeepConsistencyCheck {
        lde_trace_merkle_proofs,
        lde_composition_poly_proofs,
        lde_trace_evaluations,
        lde_composition_poly_evaluations,
    }
}

// FIXME remove unwrap() calls and return errors
pub fn prove<F: IsFFTField, A: AIR<Field = F>>(trace: &TraceTable<F>, air: &A) -> StarkProof<F>
where
    FieldElement<F>: ByteConversion,
{
    info!("Starting proof generation...");

    #[cfg(debug_assertions)]
    trace.validate(air);

    let domain = Domain::new(air);

    let mut transcript = round_0_transcript_initialization();

    // Round 1
    let round_1_result = round_1_randomized_air_with_preprocessing(trace, &domain);

    for root in round_1_result.lde_trace_merkle_roots.iter() {
        transcript.append(&root.to_bytes_be());
    }

    // Round 2
    // These are the challenges alpha^B_j and beta^B_j
    let boundary_coeffs_alphas =
        batch_sample_challenges(round_1_result.trace_polys.len(), &mut transcript);
    let boundary_coeffs_betas =
        batch_sample_challenges(round_1_result.trace_polys.len(), &mut transcript);
    let boundary_coeffs: Vec<_> = boundary_coeffs_alphas
        .into_iter()
        .zip(boundary_coeffs_betas)
        .collect();

    // These are the challenges alpha^T_j and beta^T_j
    let transition_coeffs_alphas =
        batch_sample_challenges(air.context().num_transition_constraints, &mut transcript);
    let transition_coeffs_betas =
        batch_sample_challenges(air.context().num_transition_constraints, &mut transcript);
    let transition_coeffs: Vec<_> = transition_coeffs_alphas
        .into_iter()
        .zip(transition_coeffs_betas)
        .collect();
    let round_2_result = round_2_compute_composition_polynomial(
        air,
        &domain,
        &round_1_result,
        &transition_coeffs,
        &boundary_coeffs,
    );

    transcript.append(&round_2_result.composition_poly_roots[0].to_bytes_be());
    transcript.append(&round_2_result.composition_poly_roots[1].to_bytes_be());

    // Round 3
    let z = sample_z_ood(
        &domain.lde_roots_of_unity_coset,
        &domain.trace_roots_of_unity,
        &mut transcript,
    );

    let round_3_result = round_3_evaluate_polynomials_in_out_of_domain_element(
        air,
        &domain,
        &round_1_result,
        &round_2_result,
        &z,
    );

    // H_1(z^2)
    transcript.append(&round_3_result.composition_poly_ood_evaluations[0].to_bytes_be());
    // H_2(z^2)
    transcript.append(&round_3_result.composition_poly_ood_evaluations[1].to_bytes_be());
    // These are the values t_j(z)
    for element in round_3_result.trace_ood_frame_evaluations.get_row(0).iter() {
        transcript.append(&element.to_bytes_be());
    }
    // These are the values t_j(gz)
    for element in round_3_result.trace_ood_frame_evaluations.get_row(1).iter() {
        transcript.append(&element.to_bytes_be());
    }

    // Round 4
    let round_4_result = round_4_compute_and_run_fri_on_the_deep_composition_polynomial(
        air,
        &domain,
        &round_1_result,
        &round_2_result,
        &z,
        &mut transcript,
    );

    info!("End proof generation");

    StarkProof {
        lde_trace_merkle_roots: round_1_result.lde_trace_merkle_roots,
        composition_poly_roots: round_2_result.composition_poly_roots,
        fri_layers_merkle_roots: round_4_result.fri_layers_merkle_roots,
        fri_last_value: round_4_result.fri_last_value,
        trace_ood_frame_evaluations: round_3_result.trace_ood_frame_evaluations,
        composition_poly_ood_evaluations: round_3_result.composition_poly_ood_evaluations,
        deep_consistency_check: round_4_result.deep_consistency_check,
        query_list: round_4_result.query_list,
    }
}

#[cfg(test)]
mod tests {
    use lambdaworks_math::field::{
        element::FieldElement, fields::fft_friendly::stark_252_prime_field::Stark252PrimeField,
        traits::IsFFTField,
    };

    use crate::{
        air::{
            context::{AirContext, ProofOptions},
            example::simple_fibonacci,
            trace::TraceTable,
        },
        Domain,
    };

    use super::evaluate_polynomial_on_lde_domain;

    pub type FE = FieldElement<Stark252PrimeField>;

    #[test]
    fn test_domain_constructor() {
        let trace = simple_fibonacci::fibonacci_trace([FE::from(1), FE::from(1)], 8);
        let trace_length = trace[0].len();
        let trace_table = TraceTable::new_from_cols(&trace);
        let coset_offset = 3;
        let blowup_factor: usize = 2;

        let context = AirContext {
            options: ProofOptions {
                blowup_factor: blowup_factor as u8,
                fri_number_of_queries: 1,
                coset_offset,
            },
            trace_length,
            trace_columns: trace_table.n_cols,
            transition_degrees: vec![1],
            transition_exemptions: vec![2],
            transition_offsets: vec![0, 1, 2],
            num_transition_constraints: 1,
        };

        let domain = Domain::new(&simple_fibonacci::FibonacciAIR::from(context));
        assert_eq!(domain.blowup_factor, 2);
        assert_eq!(domain.interpolation_domain_size, trace_length);
        assert_eq!(domain.root_order, trace_length.trailing_zeros());
        assert_eq!(
            domain.lde_root_order,
            (trace_length * blowup_factor).trailing_zeros()
        );
        assert_eq!(domain.coset_offset, FieldElement::from(coset_offset));

        let primitive_root = Stark252PrimeField::get_primitive_root_of_unity(
            (trace_length * blowup_factor).trailing_zeros() as u64,
        )
        .unwrap();

        assert_eq!(
            domain.trace_primitive_root,
            primitive_root.pow(blowup_factor)
        );
        for i in 0..(trace_length * blowup_factor) {
            assert_eq!(
                domain.lde_roots_of_unity_coset[i],
                FieldElement::from(coset_offset) * primitive_root.pow(i)
            );
        }
    }

    #[test]
    fn test_evaluate_polynomial_on_lde_domain() {
        let trace = simple_fibonacci::fibonacci_trace([FE::from(1), FE::from(1)], 8);
        let trace_length = trace[0].len();
        let trace_table = TraceTable::new_from_cols(&trace);
        let trace_polys = trace_table.compute_trace_polys();
        let coset_offset = 3;
        let blowup_factor: usize = 2;

        let context = AirContext {
            options: ProofOptions {
                blowup_factor: blowup_factor as u8,
                fri_number_of_queries: 1,
                coset_offset,
            },
            trace_length,
            trace_columns: trace_table.n_cols,
            transition_degrees: vec![1],
            transition_exemptions: vec![2],
            transition_offsets: vec![0, 1, 2],
            num_transition_constraints: 1,
        };
        let coset_offset = FieldElement::from(coset_offset);

        let domain = Domain::new(&simple_fibonacci::FibonacciAIR::from(context));
        let primitive_root = Stark252PrimeField::get_primitive_root_of_unity(
            (trace_length * blowup_factor).trailing_zeros() as u64,
        )
        .unwrap();

        for poly in trace_polys.iter() {
            let lde_evaluation = evaluate_polynomial_on_lde_domain(poly, &domain).unwrap();
            for i in 0..domain.interpolation_domain_size {
                assert_eq!(
                    lde_evaluation[i],
                    poly.evaluate(&(&coset_offset * primitive_root.pow(i)))
                );
            }
        }
    }
}
