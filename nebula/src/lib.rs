mod gpu;
mod renderer;

use pyo3::prelude::*;

use renderer::Renderer;

#[pymodule]
fn nebula(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Renderer>()?;
    Ok(())
}
