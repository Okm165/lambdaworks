mod crypto;
mod math;
mod merkle_tree;
mod proving_systems;

use crate::math::unsigned_integer::element::PyU256;
use crate::merkle_tree::merkle::{PyU64FE, PyU64MerkleTree};
use crate::proving_systems::stark;
use crate::proving_systems::stark::PyFieldElement;
use crate::proving_systems::stark::PyProofConfig;
use crate::proving_systems::stark::PyStarkProof;

use crypto::merkle_tree::proof::PyU64Proof;
use pyo3::prelude::*;
use pyo3::wrap_pyfunction;

#[pymodule]
fn lambdaworks_py(_py: Python, m: &PyModule) -> PyResult<()> {
    m.add_class::<PyU256>()?;
    m.add_class::<PyFieldElement>()?;
    m.add_class::<PyProofConfig>()?;
    m.add_class::<PyStarkProof>()?;
    m.add_class::<PyU64MerkleTree>()?;
    m.add_class::<PyU64Proof>()?;
    m.add_class::<PyU64FE>()?;
    m.add_function(wrap_pyfunction!(stark::prove, m)?)?;
    m.add_function(wrap_pyfunction!(stark::verify, m)?)?;
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;

    use pyo3::Python;

    #[test]
    fn lambdaworks_py_test() {
        pyo3::prepare_freethreaded_python();
        Python::with_gil(|py| {
            let module = PyModule::new(py, "lambdaworks_py");
            assert!(lambdaworks_py(py, module.unwrap()).is_ok());
        });
    }
}
