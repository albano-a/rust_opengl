mod camera;
mod colormap;
mod font;
mod geometry;
mod gpu_setup;
mod pipelines;
mod renderer;
mod slice_api;
mod spatial;
mod text;
mod text_api;
mod uniforms;
mod volume;
mod volume_api;

use pyo3::prelude::*;

use renderer::Renderer;

#[pymodule]
fn nebula(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Renderer>()?;
    Ok(())
}
