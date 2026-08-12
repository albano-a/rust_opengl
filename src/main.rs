use std::num::NonZeroU32;

use glutin::config::ConfigTemplateBuilder;
use glutin::context::{ContextApi, ContextAttributesBuilder, Version};
use glutin::display::GetGlDisplay;
use glutin::prelude::*;
use glutin::surface::{SurfaceAttributesBuilder, WindowSurface};
use glutin_winit::DisplayBuilder;
use raw_window_handle::HasWindowHandle;
use winit::{
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    window::Window,
};

fn main() {
    let event_loop = EventLoop::new().unwrap();

    let window_attributes = Window::default_attributes()
        .with_title("OpenGL")
        .with_inner_size(winit::dpi::LogicalSize::new(800, 600));

    let template = ConfigTemplateBuilder::new();

    let display_builder = DisplayBuilder::new().with_window_attributes(Some(window_attributes));

    let (window, gl_config) = display_builder
        .build(&event_loop, template, |configs| {
            configs
                .reduce(|acc, config| {
                    if config.num_samples() > acc.num_samples() {
                        config
                    } else {
                        acc
                    }
                })
                .unwrap()
        })
        .unwrap();

    let window = window.unwrap();

    let raw_window_handle = window.window_handle().unwrap().as_raw();

    let context_attributes = ContextAttributesBuilder::new()
        .with_context_api(ContextApi::OpenGl(Some(Version::new(4, 6))))
        .build(Some(raw_window_handle));

    let not_current_context = unsafe {
        gl_config
            .display()
            .create_context(&gl_config, &context_attributes)
            .unwrap()
    };

    let width = NonZeroU32::new(800).unwrap();
    let height = NonZeroU32::new(600).unwrap();

    let surface_attributes =
        SurfaceAttributesBuilder::<WindowSurface>::new().build(raw_window_handle, width, height);

    let surface = unsafe {
        gl_config
            .display()
            .create_window_surface(&gl_config, &surface_attributes)
            .unwrap()
    };

    let context = not_current_context.make_current(&surface).unwrap();

    gl::load_with(|name| {
        let name = std::ffi::CString::new(name).unwrap();

        gl_config.display().get_proc_address(&name).cast()
    });

    unsafe {
        // Define qual cor será usada quando eu
        // mandar limpar o framebuffer
        gl::ClearColor(0.1, 0.2, 0.3, 1.0);
    }

    unsafe {
        // Realmente limpa o framebuffer
        gl::Clear(gl::COLOR_BUFFER_BIT);
    }

    let vertices: [f32; 6] = [0.0, 0.5, -0.5, -0.5, 0.5, -0.5];

    // Vertex Buffer Object
    let mut vbo = 0;

    unsafe {
        gl::GenBuffers(1, &mut vbo);
        gl::BindBuffer(gl::ARRAY_BUFFER, vbo);

        gl::BufferData(
            gl::ARRAY_BUFFER,
            (vertices.len() * std::mem::size_of::<f32>()) as isize,
            vertices.as_ptr() as *const _,
            gl::STATIC_DRAW,
        );
    }

    // Vertex Array Object
    let mut vao = 0;

    unsafe {
        gl::GenVertexArrays(1, &mut vao);
        gl::BindVertexArray(vao);
    }

    unsafe {
        gl::VertexAttribPointer(
            0,
            2,
            gl::FLOAT,
            gl::FALSE,
            (2 * std::mem::size_of::<f32>()) as i32,
            std::ptr::null(),
        );

        gl::EnableVertexAttribArray(0);
    }

    let vertex_source = std::fs::read_to_string("src/shader.vert").unwrap();

    let fragment_source = std::fs::read_to_string("src/shader.frag").unwrap();

    let vertex_shader;

    unsafe {
        vertex_shader = gl::CreateShader(gl::VERTEX_SHADER);

        let source = std::ffi::CString::new(vertex_source).unwrap();

        gl::ShaderSource(vertex_shader, 1, &source.as_ptr(), std::ptr::null());

        gl::CompileShader(vertex_shader);
    }

    let fragment_shader;

    unsafe {
        fragment_shader = gl::CreateShader(gl::FRAGMENT_SHADER);

        let source = std::ffi::CString::new(fragment_source).unwrap();

        gl::ShaderSource(fragment_shader, 1, &source.as_ptr(), std::ptr::null());

        gl::CompileShader(fragment_shader);
    }

    let program;

    unsafe {
        program = gl::CreateProgram();

        gl::AttachShader(program, vertex_shader);
        gl::AttachShader(program, fragment_shader);

        gl::LinkProgram(program);

        gl::DeleteShader(vertex_shader);
        gl::DeleteShader(fragment_shader);
    }

    println!("OpenGL context created");

    event_loop
        .run(move |event, elwt| {
            elwt.set_control_flow(ControlFlow::Wait);

            match event {
                Event::AboutToWait => {
                    unsafe {
                        gl::Clear(gl::COLOR_BUFFER_BIT);

                        gl::UseProgram(program);
                        gl::BindVertexArray(vao);

                        gl::DrawArrays(gl::TRIANGLES, 0, 3);
                    }

                    surface.swap_buffers(&context).unwrap();
                }

                Event::WindowEvent {
                    event: WindowEvent::CloseRequested,
                    ..
                } => {
                    elwt.exit();
                }

                _ => {}
            }
        })
        .unwrap();
}
