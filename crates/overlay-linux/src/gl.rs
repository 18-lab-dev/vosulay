//! GL 3.2 / ES3 CPU-ring composition with persistent context-owned resources.
//! Packed premultiplied BGRA is uploaded as RGBA and shader-swizzled; draw-buffer
//! zero's indexed blend/mask state and every other touched capability are restored.
use libc::{c_char, c_int, c_uint, c_void};
use std::ffi::CStr;
use std::mem;
use std::ptr;
use vosulay_frame::DamageRect;

const GL_TEXTURE_2D: c_uint = 0x0de1;
const GL_RGBA: c_uint = 0x1908;
const GL_RGBA8: c_int = 0x8058;
const GL_UNSIGNED_BYTE: c_uint = 0x1401;
const GL_TEXTURE0: c_uint = 0x84c0;
const GL_TEXTURE_MIN_FILTER: c_uint = 0x2801;
const GL_TEXTURE_MAG_FILTER: c_uint = 0x2800;
const GL_TEXTURE_WRAP_S: c_uint = 0x2802;
const GL_TEXTURE_WRAP_T: c_uint = 0x2803;
const GL_LINEAR: c_int = 0x2601;
const GL_CLAMP_TO_EDGE: c_int = 0x812f;
const GL_VERTEX_SHADER: c_uint = 0x8b31;
const GL_FRAGMENT_SHADER: c_uint = 0x8b30;
const GL_COMPILE_STATUS: c_uint = 0x8b81;
const GL_LINK_STATUS: c_uint = 0x8b82;
const GL_CURRENT_PROGRAM: c_uint = 0x8b8d;
const GL_ACTIVE_TEXTURE: c_uint = 0x84e0;
const GL_TEXTURE_BINDING_2D: c_uint = 0x8069;
const GL_SAMPLER_BINDING: c_uint = 0x8919;
const GL_VERTEX_ARRAY_BINDING: c_uint = 0x85b5;
const GL_PIXEL_UNPACK_BUFFER_BINDING: c_uint = 0x88ef;
const GL_DRAW_FRAMEBUFFER_BINDING: c_uint = 0x8ca6;
const GL_DRAW_FRAMEBUFFER: c_uint = 0x8ca9;
const GL_DRAW_BUFFER0: c_uint = 0x8825;
const GL_BACK: c_uint = 0x0405;
const GL_PIXEL_UNPACK_BUFFER: c_uint = 0x88ec;
const GL_STREAM_DRAW: c_uint = 0x88e0;
const GL_MAP_WRITE_BIT: c_uint = 0x0002;
const GL_MAP_INVALIDATE_BUFFER_BIT: c_uint = 0x0008;
const GL_VIEWPORT: c_uint = 0x0ba2;
const GL_BLEND: c_uint = 0x0be2;
const GL_DEPTH_TEST: c_uint = 0x0b71;
const GL_STENCIL_TEST: c_uint = 0x0b90;
const GL_CULL_FACE: c_uint = 0x0b44;
const GL_SCISSOR_TEST: c_uint = 0x0c11;
const GL_RASTERIZER_DISCARD: c_uint = 0x8c89;
const GL_FRAMEBUFFER_SRGB: c_uint = 0x8db9;
const GL_DITHER: c_uint = 0x0bd0;
const GL_SAMPLE_ALPHA_TO_COVERAGE: c_uint = 0x809e;
const GL_SAMPLE_COVERAGE: c_uint = 0x80a0;
const GL_SAMPLE_MASK: c_uint = 0x8e51;
const GL_COLOR_LOGIC_OP: c_uint = 0x0bf2;
const GL_BLEND_SRC_RGB: c_uint = 0x80c9;
const GL_BLEND_DST_RGB: c_uint = 0x80c8;
const GL_BLEND_SRC_ALPHA: c_uint = 0x80cb;
const GL_BLEND_DST_ALPHA: c_uint = 0x80ca;
const GL_BLEND_EQUATION_RGB: c_uint = 0x8009;
const GL_BLEND_EQUATION_ALPHA: c_uint = 0x883d;
const GL_COLOR_WRITEMASK: c_uint = 0x0c23;
const GL_UNPACK_ALIGNMENT: c_uint = 0x0cf5;
const GL_UNPACK_ROW_LENGTH: c_uint = 0x0cf2;
const GL_UNPACK_SKIP_ROWS: c_uint = 0x0cf3;
const GL_UNPACK_SKIP_PIXELS: c_uint = 0x0cf4;
const GL_FUNC_ADD: c_uint = 0x8006;
const GL_ONE: c_uint = 1;
const GL_ONE_MINUS_SRC_ALPHA: c_uint = 0x0303;
const GL_TRIANGLES: c_uint = 0x0004;
const GL_VERSION: c_uint = 0x1f02;

const DESKTOP_VERTEX: &[u8] = b"#version 150\nout vec2 uv;\nvoid main(){vec2 p=vec2((gl_VertexID<<1)&2,gl_VertexID&2);uv=vec2(p.x,1.0-p.y);gl_Position=vec4(p*2.0-1.0,0.0,1.0);}\0";
const DESKTOP_FRAGMENT: &[u8] = b"#version 150\nin vec2 uv;uniform sampler2D overlay_texture;out vec4 color;\nvoid main(){color=texture(overlay_texture,uv).bgra;}\0";
const ES_VERTEX: &[u8] = b"#version 300 es\nprecision highp float;out vec2 uv;\nvoid main(){vec2 p=vec2((gl_VertexID<<1)&2,gl_VertexID&2);uv=vec2(p.x,1.0-p.y);gl_Position=vec4(p*2.0-1.0,0.0,1.0);}\0";
const ES_FRAGMENT: &[u8] = b"#version 300 es\nprecision mediump float;in vec2 uv;uniform sampler2D overlay_texture;out vec4 color;\nvoid main(){color=texture(overlay_texture,uv).bgra;}\0";

type GetString = unsafe extern "C" fn(c_uint) -> *const u8;
type GetIntegerv = unsafe extern "C" fn(c_uint, *mut c_int);
type GetBooleanv = unsafe extern "C" fn(c_uint, *mut u8);
type IsEnabled = unsafe extern "C" fn(c_uint) -> u8;
type Enable = unsafe extern "C" fn(c_uint);
type Disable = unsafe extern "C" fn(c_uint);
type Viewport = unsafe extern "C" fn(c_int, c_int, c_int, c_int);
type GenTextures = unsafe extern "C" fn(c_int, *mut c_uint);
type DeleteTextures = unsafe extern "C" fn(c_int, *const c_uint);
type BindTexture = unsafe extern "C" fn(c_uint, c_uint);
type TexParameteri = unsafe extern "C" fn(c_uint, c_uint, c_int);
type TexImage2D =
    unsafe extern "C" fn(c_uint, c_int, c_int, c_int, c_int, c_int, c_uint, c_uint, *const c_void);
type TexSubImage2D =
    unsafe extern "C" fn(c_uint, c_int, c_int, c_int, c_int, c_int, c_uint, c_uint, *const c_void);
type ActiveTexture = unsafe extern "C" fn(c_uint);
type CreateShader = unsafe extern "C" fn(c_uint) -> c_uint;
type ShaderSource = unsafe extern "C" fn(c_uint, c_int, *const *const c_char, *const c_int);
type CompileShader = unsafe extern "C" fn(c_uint);
type GetShaderiv = unsafe extern "C" fn(c_uint, c_uint, *mut c_int);
type CreateProgram = unsafe extern "C" fn() -> c_uint;
type AttachShader = unsafe extern "C" fn(c_uint, c_uint);
type LinkProgram = unsafe extern "C" fn(c_uint);
type GetProgramiv = unsafe extern "C" fn(c_uint, c_uint, *mut c_int);
type DeleteShader = unsafe extern "C" fn(c_uint);
type DeleteProgram = unsafe extern "C" fn(c_uint);
type UseProgram = unsafe extern "C" fn(c_uint);
type GetUniformLocation = unsafe extern "C" fn(c_uint, *const c_char) -> c_int;
type Uniform1i = unsafe extern "C" fn(c_int, c_int);
type GenVertexArrays = unsafe extern "C" fn(c_int, *mut c_uint);
type DeleteVertexArrays = unsafe extern "C" fn(c_int, *const c_uint);
type BindVertexArray = unsafe extern "C" fn(c_uint);
type BindBuffer = unsafe extern "C" fn(c_uint, c_uint);
type BindFramebuffer = unsafe extern "C" fn(c_uint, c_uint);
type BindSampler = unsafe extern "C" fn(c_uint, c_uint);
type DrawBuffers = unsafe extern "C" fn(c_int, *const c_uint);
type PixelStorei = unsafe extern "C" fn(c_uint, c_int);
type GenBuffers = unsafe extern "C" fn(c_int, *mut c_uint);
type DeleteBuffers = unsafe extern "C" fn(c_int, *const c_uint);
type BufferData = unsafe extern "C" fn(c_uint, isize, *const c_void, c_uint);
type MapBufferRange = unsafe extern "C" fn(c_uint, isize, isize, c_uint) -> *mut c_void;
type UnmapBuffer = unsafe extern "C" fn(c_uint) -> u8;
type BlendFuncSeparate = unsafe extern "C" fn(c_uint, c_uint, c_uint, c_uint);
type BlendEquationSeparate = unsafe extern "C" fn(c_uint, c_uint);
type ColorMask = unsafe extern "C" fn(u8, u8, u8, u8);
type DrawArrays = unsafe extern "C" fn(c_uint, c_int, c_int);
type PolygonMode = unsafe extern "C" fn(c_uint, c_uint);
type GetStringi = unsafe extern "C" fn(c_uint, c_uint) -> *const u8;
type Enablei = unsafe extern "C" fn(c_uint, c_uint);
type ColorMaski = unsafe extern "C" fn(c_uint, u8, u8, u8, u8);
type BlendFuncSeparatei = unsafe extern "C" fn(c_uint, c_uint, c_uint, c_uint, c_uint);
type BlendEquationSeparatei = unsafe extern "C" fn(c_uint, c_uint, c_uint);

#[derive(Clone, Copy)]
struct Gl {
    get_string: GetString,
    get_integer: GetIntegerv,
    get_boolean: GetBooleanv,
    polygon_mode: Option<PolygonMode>,
    has_sample_mask: bool,
    has_tf_query: bool,
    has_indexed_state: bool,
    has_indexed_blend: bool,
    has_sampler_objects: bool,
    is_compat: bool,
    get_string_i: GetStringi,
    enable_i: Option<Enablei>,
    disable_i: Option<Enablei>,
    color_mask_i: Option<ColorMaski>,
    blend_func_i: Option<BlendFuncSeparatei>,
    blend_equation_i: Option<BlendEquationSeparatei>,
    is_enabled: IsEnabled,
    enable: Enable,
    disable: Disable,
    viewport: Viewport,
    gen_textures: GenTextures,
    delete_textures: DeleteTextures,
    bind_texture: BindTexture,
    tex_parameter: TexParameteri,
    tex_image: TexImage2D,
    tex_sub_image: TexSubImage2D,
    active_texture: ActiveTexture,
    create_shader: CreateShader,
    shader_source: ShaderSource,
    compile_shader: CompileShader,
    get_shader: GetShaderiv,
    create_program: CreateProgram,
    attach_shader: AttachShader,
    link_program: LinkProgram,
    get_program: GetProgramiv,
    delete_shader: DeleteShader,
    delete_program: DeleteProgram,
    use_program: UseProgram,
    uniform_location: GetUniformLocation,
    uniform_1i: Uniform1i,
    gen_vertex_arrays: GenVertexArrays,
    delete_vertex_arrays: DeleteVertexArrays,
    bind_vertex_array: BindVertexArray,
    bind_buffer: BindBuffer,
    bind_framebuffer: BindFramebuffer,
    bind_sampler: Option<BindSampler>,
    draw_buffers: DrawBuffers,
    pixel_store: PixelStorei,
    gen_buffers: GenBuffers,
    delete_buffers: DeleteBuffers,
    buffer_data: BufferData,
    map_buffer_range: MapBufferRange,
    unmap_buffer: UnmapBuffer,
    blend_func_separate: BlendFuncSeparate,
    blend_equation_separate: BlendEquationSeparate,
    color_mask: ColorMask,
    draw_arrays: DrawArrays,
}

impl Gl {
    unsafe fn load(mut load: impl FnMut(&[u8]) -> *const c_void) -> Option<Self> {
        macro_rules! symbol {
            ($name:literal, $kind:ty) => {{
                let address = load(concat!($name, "\0").as_bytes());
                if address.is_null() {
                    return None;
                }
                // SAFETY: GL resolves the named function with this C ABI.
                unsafe { mem::transmute::<*const c_void, $kind>(address) }
            }};
        }
        macro_rules! optional {
            ($name:literal, $kind:ty) => {{
                let mut address = load(concat!($name, "\0").as_bytes());
                if address.is_null() {
                    address = load(concat!($name, "EXT\0").as_bytes());
                }
                if address.is_null() {
                    address = load(concat!($name, "OES\0").as_bytes());
                }
                if address.is_null() {
                    address = load(concat!($name, "ARB\0").as_bytes());
                }
                if address.is_null() {
                    None
                } else {
                    Some(unsafe { mem::transmute::<*const c_void, $kind>(address) })
                }
            }};
        }
        Some(Self {
            has_sample_mask: false,
            has_tf_query: false,
            has_indexed_state: false,
            has_indexed_blend: false,
            has_sampler_objects: false,
            is_compat: false,
            get_string_i: symbol!("glGetStringi", GetStringi),
            enable_i: optional!("glEnablei", Enablei),
            disable_i: optional!("glDisablei", Enablei),
            color_mask_i: optional!("glColorMaski", ColorMaski),
            blend_func_i: optional!("glBlendFuncSeparatei", BlendFuncSeparatei),
            blend_equation_i: optional!("glBlendEquationSeparatei", BlendEquationSeparatei),
            bind_sampler: optional!("glBindSampler", BindSampler),
            get_string: symbol!("glGetString", GetString),
            get_integer: symbol!("glGetIntegerv", GetIntegerv),
            get_boolean: symbol!("glGetBooleanv", GetBooleanv),
            is_enabled: symbol!("glIsEnabled", IsEnabled),
            enable: symbol!("glEnable", Enable),
            disable: symbol!("glDisable", Disable),
            viewport: symbol!("glViewport", Viewport),
            gen_textures: symbol!("glGenTextures", GenTextures),
            delete_textures: symbol!("glDeleteTextures", DeleteTextures),
            bind_texture: symbol!("glBindTexture", BindTexture),
            tex_parameter: symbol!("glTexParameteri", TexParameteri),
            tex_image: symbol!("glTexImage2D", TexImage2D),
            tex_sub_image: symbol!("glTexSubImage2D", TexSubImage2D),
            active_texture: symbol!("glActiveTexture", ActiveTexture),
            create_shader: symbol!("glCreateShader", CreateShader),
            shader_source: symbol!("glShaderSource", ShaderSource),
            compile_shader: symbol!("glCompileShader", CompileShader),
            get_shader: symbol!("glGetShaderiv", GetShaderiv),
            create_program: symbol!("glCreateProgram", CreateProgram),
            attach_shader: symbol!("glAttachShader", AttachShader),
            link_program: symbol!("glLinkProgram", LinkProgram),
            get_program: symbol!("glGetProgramiv", GetProgramiv),
            delete_shader: symbol!("glDeleteShader", DeleteShader),
            delete_program: symbol!("glDeleteProgram", DeleteProgram),
            use_program: symbol!("glUseProgram", UseProgram),
            uniform_location: symbol!("glGetUniformLocation", GetUniformLocation),
            uniform_1i: symbol!("glUniform1i", Uniform1i),
            gen_vertex_arrays: symbol!("glGenVertexArrays", GenVertexArrays),
            delete_vertex_arrays: symbol!("glDeleteVertexArrays", DeleteVertexArrays),
            bind_vertex_array: symbol!("glBindVertexArray", BindVertexArray),
            bind_buffer: symbol!("glBindBuffer", BindBuffer),
            bind_framebuffer: symbol!("glBindFramebuffer", BindFramebuffer),
            draw_buffers: symbol!("glDrawBuffers", DrawBuffers),
            pixel_store: symbol!("glPixelStorei", PixelStorei),
            gen_buffers: symbol!("glGenBuffers", GenBuffers),
            delete_buffers: symbol!("glDeleteBuffers", DeleteBuffers),
            buffer_data: symbol!("glBufferData", BufferData),
            map_buffer_range: symbol!("glMapBufferRange", MapBufferRange),
            unmap_buffer: symbol!("glUnmapBuffer", UnmapBuffer),
            blend_func_separate: symbol!("glBlendFuncSeparate", BlendFuncSeparate),
            blend_equation_separate: symbol!("glBlendEquationSeparate", BlendEquationSeparate),
            color_mask: symbol!("glColorMask", ColorMask),
            draw_arrays: symbol!("glDrawArrays", DrawArrays),
            polygon_mode: {
                let p = load(b"glPolygonMode\0");
                if p.is_null() {
                    None
                } else {
                    Some(unsafe { mem::transmute::<*const c_void, PolygonMode>(p) })
                }
            },
        })
    }
}

pub(crate) struct Renderer {
    context: usize,
    gl: Gl,
    texture: c_uint,
    program: c_uint,
    vao: c_uint,
    /// Pixel unpack buffer that changed overlay rows are staged in.
    staging: c_uint,
    width: u32,
    height: u32,
    has_frame: bool,
    sequence: u64,
    is_es: bool,
    max_texture_size: u32,
}

impl Renderer {
    pub(crate) unsafe fn new(
        context: usize,
        width: u32,
        height: u32,
        load: impl FnMut(&[u8]) -> *const c_void,
    ) -> Option<Self> {
        // SAFETY: caller guarantees a current GL3/ES3 context for this loader.
        let mut gl = unsafe { Gl::load(load) }?;
        // SAFETY: current context owns the returned static version string.
        let version = unsafe { (gl.get_string)(GL_VERSION) };
        if version.is_null() {
            return None;
        }
        // SAFETY: GL_VERSION is a NUL-terminated context-owned string.
        let version = unsafe { CStr::from_ptr(version.cast()) }.to_bytes();
        let is_es = version.windows(9).any(|window| window == b"OpenGL ES");
        let number = if is_es {
            version.strip_prefix(b"OpenGL ES ")?
        } else {
            version
        };
        if number.len() < 3
            || !number[0].is_ascii_digit()
            || number[1] != b'.'
            || !number[2].is_ascii_digit()
        {
            return None;
        }
        let major = number[0] - b'0';
        let minor = number[2] - b'0';
        if major < 3 || (!is_es && major == 3 && minor < 2) || (!is_es && gl.polygon_mode.is_none())
        {
            return None;
        }
        gl.has_sample_mask = !is_es || major > 3 || minor >= 1;
        gl.has_tf_query = is_es || major >= 4;
        if !is_es {
            let mut profile = 0;
            unsafe {
                (gl.get_integer)(0x9126, &mut profile);
            }
            gl.is_compat = profile & 2 != 0;
        }
        let mut extension_count = 0;
        unsafe {
            (gl.get_integer)(0x821d, &mut extension_count);
        }
        let mut indexed_extension = false;
        let mut blend_extension = false;
        let mut sampler_extension = false;
        for index in 0..extension_count.clamp(0, 4096) as u32 {
            let extension = unsafe { (gl.get_string_i)(0x1f03, index) };
            if extension.is_null() {
                continue;
            }
            let extension = unsafe { CStr::from_ptr(extension.cast()) }.to_bytes();
            indexed_extension |= extension == b"GL_EXT_draw_buffers_indexed"
                || extension == b"GL_OES_draw_buffers_indexed";
            blend_extension |= extension == b"GL_ARB_draw_buffers_blend";
            sampler_extension |= extension == b"GL_ARB_sampler_objects";
            gl.has_tf_query |= extension == b"GL_ARB_transform_feedback2";
        }
        gl.has_sampler_objects =
            is_es || major > 3 || (major == 3 && minor >= 3) || sampler_extension;
        if gl.has_sampler_objects && gl.bind_sampler.is_none() {
            return None;
        }
        gl.has_indexed_state = !is_es || major > 3 || minor >= 2 || indexed_extension;
        gl.has_indexed_blend = if is_es {
            gl.has_indexed_state
        } else {
            major >= 4 || blend_extension
        };
        if gl.has_indexed_state
            && (gl.enable_i.is_none() || gl.disable_i.is_none() || gl.color_mask_i.is_none())
        {
            return None;
        }
        if gl.has_indexed_blend && (gl.blend_func_i.is_none() || gl.blend_equation_i.is_none()) {
            return None;
        }
        let mut maximum = 0;
        unsafe {
            (gl.get_integer)(0x0d33, &mut maximum);
        }
        if maximum <= 0
            || width == 0
            || height == 0
            || width > maximum as u32
            || height > maximum as u32
        {
            return None;
        }
        let mut state = GlState::capture(&gl, is_es, true);
        let resources = unsafe {
            create_resources(
                &gl,
                if is_es { ES_VERTEX } else { DESKTOP_VERTEX },
                if is_es { ES_FRAGMENT } else { DESKTOP_FRAGMENT },
                width,
                height,
            )
        };
        // SAFETY: capture and restore use the same current context.
        unsafe {
            state.restore(&gl, is_es);
        }
        let (texture, program, vao) = resources?;
        let mut staging = 0;
        // SAFETY: the loader's context is current; zero means no buffer.
        unsafe {
            (gl.gen_buffers)(1, &mut staging);
        }
        Some(Self {
            context,
            gl,
            texture,
            staging,
            program,
            vao,
            width,
            height,
            has_frame: false,
            sequence: 0,
            is_es,
            max_texture_size: maximum as u32,
        })
    }

    pub(crate) fn matches(&self, context: usize) -> bool {
        self.context == context
    }
    pub(crate) fn has_frame(&self) -> bool {
        self.has_frame
    }
    pub(crate) fn sequence(&self) -> u64 {
        self.sequence
    }

    pub(crate) fn invalidate(&mut self) {
        self.has_frame = false;
    }

    pub(crate) unsafe fn resize(&mut self, width: u32, height: u32) {
        if self.width == width && self.height == height {
            return;
        }
        let mut state = GlState::capture(&self.gl, self.is_es, true);
        // SAFETY: renderer context is current and the texture is owned by it.
        unsafe {
            (self.gl.active_texture)(GL_TEXTURE0);
            (self.gl.bind_texture)(GL_TEXTURE_2D, self.texture);
            set_unpack_defaults(&self.gl);
            (self.gl.tex_image)(
                GL_TEXTURE_2D,
                0,
                GL_RGBA8,
                width as c_int,
                height as c_int,
                0,
                GL_RGBA,
                GL_UNSIGNED_BYTE,
                ptr::null(),
            );
            state.restore(&self.gl, self.is_es);
        }
        self.width = width;
        self.height = height;
        self.has_frame = false;
    }

    pub(crate) unsafe fn resize_texture_if_needed(&mut self, width: u32, height: u32) -> bool {
        if width == 0
            || height == 0
            || width > self.max_texture_size
            || height > self.max_texture_size
        {
            return false;
        }
        unsafe {
            self.resize(width, height);
        }
        true
    }

    pub(crate) unsafe fn present_sized(
        &mut self,
        frame: Option<vosulay_transport::ReadFrame>,
        width: u32,
        height: u32,
    ) -> Option<bool> {
        unsafe { self.compose(frame, width, height) }
    }

    unsafe fn compose(
        &mut self,
        frame: Option<vosulay_transport::ReadFrame>,
        width: u32,
        height: u32,
    ) -> Option<bool> {
        if frame.is_none() && !self.has_frame {
            return Some(false);
        }
        let expected = self.width as usize * self.height as usize * 4;
        if frame
            .as_ref()
            .is_some_and(|frame| frame.pixels().len() != expected)
        {
            return None;
        }
        let gl = &self.gl;
        let mut active = 0;
        let mut paused = 0;
        if gl.has_tf_query {
            unsafe {
                (gl.get_integer)(0x8e24, &mut active);
                (gl.get_integer)(0x8e23, &mut paused);
            }
        }
        if active != 0 && paused == 0 {
            return None;
        }
        let mut state = GlState::capture(gl, self.is_es, frame.is_some());
        // SAFETY: renderer context is current and the texture belongs to it.
        unsafe {
            (gl.active_texture)(GL_TEXTURE0);
            if gl.has_sampler_objects
                && let Some(bind_sampler) = gl.bind_sampler
            {
                bind_sampler(0, 0);
            }
            (gl.bind_texture)(GL_TEXTURE_2D, self.texture);
        }
        let consumed = if let Some(frame) = frame.as_ref() {
            let bytes = frame.pixels();
            let region = self
                .has_frame
                .then(|| frame.damage_since(self.sequence))
                .flatten()
                .unwrap_or_else(|| DamageRect::full(self.width, self.height));
            // Input is tightly-packed BGRA. Uploading as RGBA is portable to
            // ES3; the fragment shader performs the B/R swizzle. Only the
            // region changed since this texture's frame is copied: a full
            // frame per present costs milliseconds at 1440p.
            unsafe {
                set_unpack_defaults(gl);
                let source = stage_rows(gl, self.staging, bytes, self.width, region);
                if source.is_none() {
                    (gl.pixel_store)(GL_UNPACK_ROW_LENGTH, self.width as c_int);
                    (gl.pixel_store)(GL_UNPACK_SKIP_PIXELS, region.x as c_int);
                    (gl.pixel_store)(GL_UNPACK_SKIP_ROWS, region.y as c_int);
                }
                (gl.tex_sub_image)(
                    GL_TEXTURE_2D,
                    0,
                    region.x as c_int,
                    region.y as c_int,
                    region.width as c_int,
                    region.height as c_int,
                    GL_RGBA,
                    GL_UNSIGNED_BYTE,
                    source.unwrap_or(bytes.as_ptr().cast()),
                );
            }
            self.has_frame = true;
            self.sequence = frame.sequence();
            true
        } else {
            false
        };
        // glTexSubImage2D has synchronously consumed client memory. Release the
        // ring slot before submitting draw work or invoking the driver's swap.
        drop(frame);
        if self.has_frame {
            // SAFETY: all state touched below is captured and restored exactly.
            unsafe {
                (gl.bind_framebuffer)(GL_DRAW_FRAMEBUFFER, 0);
                let back = if state.double_buffer { GL_BACK } else { 0x0404 };
                (gl.draw_buffers)(1, &back);
                (gl.viewport)(0, 0, width as c_int, height as c_int);
                if !self.is_es {
                    if let Some(mode) = gl.polygon_mode {
                        mode(0x0408, 0x1b02);
                    }
                    for index in 0..8 {
                        set_enabled(gl, 0x3000 + index, false);
                    }
                }
                set_enabled(gl, GL_DEPTH_TEST, false);
                set_enabled(gl, GL_STENCIL_TEST, false);
                set_enabled(gl, GL_CULL_FACE, false);
                set_enabled(gl, GL_SCISSOR_TEST, false);
                set_enabled(gl, GL_RASTERIZER_DISCARD, false);
                if !self.is_es {
                    set_enabled(gl, GL_FRAMEBUFFER_SRGB, false);
                }
                set_enabled(gl, GL_DITHER, false);
                set_enabled(gl, GL_SAMPLE_ALPHA_TO_COVERAGE, false);
                set_enabled(gl, GL_SAMPLE_COVERAGE, false);
                if gl.has_sample_mask {
                    set_enabled(gl, GL_SAMPLE_MASK, false);
                }
                if !self.is_es {
                    set_enabled(gl, GL_COLOR_LOGIC_OP, false);
                    set_enabled(gl, 0x809f, false);
                }
                if gl.is_compat {
                    set_enabled(gl, 0x0bc0, false);
                    set_enabled(gl, 0x0b42, false);
                }
                set_enabled(gl, GL_BLEND, true);
                gl.blend_function(
                    GL_ONE,
                    GL_ONE_MINUS_SRC_ALPHA,
                    GL_ONE,
                    GL_ONE_MINUS_SRC_ALPHA,
                );
                gl.blend_equation(GL_FUNC_ADD, GL_FUNC_ADD);
                gl.mask(1, 1, 1, 1);
                (gl.use_program)(self.program);
                (gl.bind_vertex_array)(self.vao);
                (gl.draw_arrays)(GL_TRIANGLES, 0, 3);
            }
        }
        // SAFETY: same current context as capture.
        unsafe {
            state.restore(gl, self.is_es);
        }
        Some(consumed)
    }

    pub(crate) unsafe fn destroy_if_current(&mut self, context: usize) {
        if self.context != context {
            return;
        }
        // SAFETY: the matching owning context is current.
        unsafe {
            (self.gl.delete_textures)(1, &self.texture);
            if self.staging != 0 {
                (self.gl.delete_buffers)(1, &self.staging);
            }
            (self.gl.delete_program)(self.program);
            (self.gl.delete_vertex_arrays)(1, &self.vao);
        }
        self.texture = 0;
        self.staging = 0;
        self.program = 0;
        self.vao = 0;
        self.has_frame = false;
    }
}

struct GlState {
    program: c_int,
    active_texture: c_int,
    texture_zero: c_int,
    sampler_zero: Option<c_int>,
    vao: c_int,
    unpack_buffer: c_int,
    unpack_touched: bool,
    draw_framebuffer: c_int,
    default_draw_buffer: c_int,
    viewport: [c_int; 4],
    polygon: [c_int; 2],
    clips: [bool; 8],
    double_buffer: bool,
    unpack_alignment: c_int,
    unpack_row_length: c_int,
    unpack_skip_rows: c_int,
    unpack_skip_pixels: c_int,
    blend_src_rgb: c_int,
    blend_dst_rgb: c_int,
    blend_src_alpha: c_int,
    blend_dst_alpha: c_int,
    blend_equation_rgb: c_int,
    blend_equation_alpha: c_int,
    color_mask: [u8; 4],
    blend: bool,
    depth: bool,
    stencil: bool,
    cull: bool,
    scissor: bool,
    rasterizer_discard: bool,
    framebuffer_srgb: bool,
    dither: bool,
    sample_alpha_to_coverage: bool,
    sample_coverage: bool,
    sample_mask: bool,
    color_logic: bool,
    alpha_test: bool,
    polygon_stipple: bool,
    sample_alpha_to_one: bool,
}

impl GlState {
    fn capture(gl: &Gl, is_es: bool, unpack_touched: bool) -> Self {
        unsafe fn integer(gl: &Gl, name: c_uint) -> c_int {
            let mut value = 0;
            // SAFETY: value points to storage of the required scalar size.
            unsafe {
                (gl.get_integer)(name, &mut value);
            }
            value
        }
        let active_texture = unsafe { integer(gl, GL_ACTIVE_TEXTURE) };
        // SAFETY: selecting a texture unit is valid in GL3/ES3.
        unsafe {
            (gl.active_texture)(GL_TEXTURE0);
        }
        let texture_zero = unsafe { integer(gl, GL_TEXTURE_BINDING_2D) };
        let sampler_zero = if gl.has_sampler_objects {
            Some(unsafe { integer(gl, GL_SAMPLER_BINDING) })
        } else {
            None
        };
        // SAFETY: restore selection immediately; bindings are unchanged.
        unsafe {
            (gl.active_texture)(active_texture as c_uint);
        }
        let mut viewport = [0; 4];
        let mut color_mask = [0; 4];
        let mut polygon = [0; 2];
        let mut clips = [false; 8];
        let mut double_buffer = 1;
        if !is_es {
            unsafe {
                (gl.get_integer)(0x0b40, polygon.as_mut_ptr());
                // Core profiles expose one front-and-back mode. Some drivers
                // write only that scalar; separate face modes are compatibility-only.
                if !gl.is_compat {
                    polygon[1] = polygon[0];
                }
                (gl.get_boolean)(0x0c32, &mut double_buffer);
                for (index, enabled) in clips.iter_mut().enumerate() {
                    *enabled = (gl.is_enabled)(0x3000 + index as u32) != 0;
                }
            }
        }
        // SAFETY: arrays provide four elements as required by these pnames.
        unsafe {
            (gl.get_integer)(GL_VIEWPORT, viewport.as_mut_ptr());
            (gl.get_boolean)(GL_COLOR_WRITEMASK, color_mask.as_mut_ptr());
        }
        let draw_framebuffer = unsafe { integer(gl, GL_DRAW_FRAMEBUFFER_BINDING) };
        let default_draw_buffer = if draw_framebuffer == 0 {
            unsafe { integer(gl, GL_DRAW_BUFFER0) }
        } else {
            // SAFETY: framebuffer zero is always a valid draw framebuffer.
            unsafe {
                (gl.bind_framebuffer)(GL_DRAW_FRAMEBUFFER, 0);
            }
            let buffer = unsafe { integer(gl, GL_DRAW_BUFFER0) };
            unsafe {
                (gl.bind_framebuffer)(GL_DRAW_FRAMEBUFFER, draw_framebuffer as c_uint);
            }
            buffer
        };
        Self {
            polygon,
            clips,
            double_buffer: double_buffer != 0,
            program: unsafe { integer(gl, GL_CURRENT_PROGRAM) },
            active_texture,
            texture_zero,
            sampler_zero,
            vao: unsafe { integer(gl, GL_VERTEX_ARRAY_BINDING) },
            unpack_touched,
            unpack_buffer: if unpack_touched {
                unsafe { integer(gl, GL_PIXEL_UNPACK_BUFFER_BINDING) }
            } else {
                0
            },
            draw_framebuffer,
            default_draw_buffer,
            viewport,
            unpack_alignment: if unpack_touched {
                unsafe { integer(gl, GL_UNPACK_ALIGNMENT) }
            } else {
                0
            },
            unpack_row_length: if unpack_touched {
                unsafe { integer(gl, GL_UNPACK_ROW_LENGTH) }
            } else {
                0
            },
            unpack_skip_rows: if unpack_touched {
                unsafe { integer(gl, GL_UNPACK_SKIP_ROWS) }
            } else {
                0
            },
            unpack_skip_pixels: if unpack_touched {
                unsafe { integer(gl, GL_UNPACK_SKIP_PIXELS) }
            } else {
                0
            },
            blend_src_rgb: unsafe { integer(gl, GL_BLEND_SRC_RGB) },
            blend_dst_rgb: unsafe { integer(gl, GL_BLEND_DST_RGB) },
            blend_src_alpha: unsafe { integer(gl, GL_BLEND_SRC_ALPHA) },
            blend_dst_alpha: unsafe { integer(gl, GL_BLEND_DST_ALPHA) },
            blend_equation_rgb: unsafe { integer(gl, GL_BLEND_EQUATION_RGB) },
            blend_equation_alpha: unsafe { integer(gl, GL_BLEND_EQUATION_ALPHA) },
            color_mask,
            // SAFETY: glIsEnabled is valid for each capability in its profile.
            blend: unsafe { (gl.is_enabled)(GL_BLEND) != 0 },
            depth: unsafe { (gl.is_enabled)(GL_DEPTH_TEST) != 0 },
            stencil: unsafe { (gl.is_enabled)(GL_STENCIL_TEST) != 0 },
            cull: unsafe { (gl.is_enabled)(GL_CULL_FACE) != 0 },
            scissor: unsafe { (gl.is_enabled)(GL_SCISSOR_TEST) != 0 },
            rasterizer_discard: unsafe { (gl.is_enabled)(GL_RASTERIZER_DISCARD) != 0 },
            framebuffer_srgb: !is_es && unsafe { (gl.is_enabled)(GL_FRAMEBUFFER_SRGB) != 0 },
            dither: unsafe { (gl.is_enabled)(GL_DITHER) != 0 },
            sample_alpha_to_coverage: unsafe { (gl.is_enabled)(GL_SAMPLE_ALPHA_TO_COVERAGE) != 0 },
            sample_coverage: unsafe { (gl.is_enabled)(GL_SAMPLE_COVERAGE) != 0 },
            sample_mask: gl.has_sample_mask && unsafe { (gl.is_enabled)(GL_SAMPLE_MASK) != 0 },
            color_logic: !is_es && unsafe { (gl.is_enabled)(GL_COLOR_LOGIC_OP) != 0 },
            alpha_test: gl.is_compat && unsafe { (gl.is_enabled)(0x0bc0) != 0 },
            polygon_stipple: gl.is_compat && unsafe { (gl.is_enabled)(0x0b42) != 0 },
            sample_alpha_to_one: !is_es && unsafe { (gl.is_enabled)(0x809f) != 0 },
        }
    }

    unsafe fn restore(&mut self, gl: &Gl, is_es: bool) {
        // SAFETY: every value was captured from this same current context.
        unsafe {
            (gl.use_program)(self.program as c_uint);
            if !is_es {
                if let Some(mode) = gl.polygon_mode {
                    if self.polygon[0] == self.polygon[1] {
                        mode(0x0408, self.polygon[0] as u32);
                    } else {
                        mode(0x0404, self.polygon[0] as u32);
                        mode(0x0405, self.polygon[1] as u32);
                    }
                }
                for (index, enabled) in self.clips.iter().enumerate() {
                    set_enabled(gl, 0x3000 + index as u32, *enabled);
                }
            }
            (gl.bind_vertex_array)(self.vao as c_uint);
            (gl.bind_framebuffer)(GL_DRAW_FRAMEBUFFER, 0);
            let default_buffer = self.default_draw_buffer as c_uint;
            (gl.draw_buffers)(1, &default_buffer);
            (gl.bind_framebuffer)(GL_DRAW_FRAMEBUFFER, self.draw_framebuffer as c_uint);
            (gl.active_texture)(GL_TEXTURE0);
            (gl.bind_texture)(GL_TEXTURE_2D, self.texture_zero as c_uint);
            if let (Some(bind_sampler), Some(sampler)) = (gl.bind_sampler, self.sampler_zero) {
                bind_sampler(0, sampler as c_uint);
            }
            (gl.active_texture)(self.active_texture as c_uint);
            if self.unpack_touched {
                (gl.bind_buffer)(GL_PIXEL_UNPACK_BUFFER, self.unpack_buffer as c_uint);
                (gl.pixel_store)(GL_UNPACK_ALIGNMENT, self.unpack_alignment);
                (gl.pixel_store)(GL_UNPACK_ROW_LENGTH, self.unpack_row_length);
                (gl.pixel_store)(GL_UNPACK_SKIP_ROWS, self.unpack_skip_rows);
                (gl.pixel_store)(GL_UNPACK_SKIP_PIXELS, self.unpack_skip_pixels);
            }
            (gl.viewport)(
                self.viewport[0],
                self.viewport[1],
                self.viewport[2],
                self.viewport[3],
            );
            gl.blend_function(
                self.blend_src_rgb as c_uint,
                self.blend_dst_rgb as c_uint,
                self.blend_src_alpha as c_uint,
                self.blend_dst_alpha as c_uint,
            );
            gl.blend_equation(
                self.blend_equation_rgb as c_uint,
                self.blend_equation_alpha as c_uint,
            );
            gl.mask(
                self.color_mask[0],
                self.color_mask[1],
                self.color_mask[2],
                self.color_mask[3],
            );
            set_enabled(gl, GL_BLEND, self.blend);
            set_enabled(gl, GL_DEPTH_TEST, self.depth);
            set_enabled(gl, GL_STENCIL_TEST, self.stencil);
            set_enabled(gl, GL_CULL_FACE, self.cull);
            set_enabled(gl, GL_SCISSOR_TEST, self.scissor);
            set_enabled(gl, GL_RASTERIZER_DISCARD, self.rasterizer_discard);
            if !is_es {
                set_enabled(gl, GL_FRAMEBUFFER_SRGB, self.framebuffer_srgb);
            }
            set_enabled(gl, GL_DITHER, self.dither);
            set_enabled(
                gl,
                GL_SAMPLE_ALPHA_TO_COVERAGE,
                self.sample_alpha_to_coverage,
            );
            set_enabled(gl, GL_SAMPLE_COVERAGE, self.sample_coverage);
            if !is_es {
                set_enabled(gl, 0x809f, self.sample_alpha_to_one);
            }
            if gl.is_compat {
                set_enabled(gl, 0x0bc0, self.alpha_test);
                set_enabled(gl, 0x0b42, self.polygon_stipple);
            }
            if gl.has_sample_mask {
                set_enabled(gl, GL_SAMPLE_MASK, self.sample_mask);
            }
            if !is_es {
                set_enabled(gl, GL_COLOR_LOGIC_OP, self.color_logic);
            }
        }
    }
}

unsafe fn create_resources(
    gl: &Gl,
    vertex_source: &[u8],
    fragment_source: &[u8],
    width: u32,
    height: u32,
) -> Option<(c_uint, c_uint, c_uint)> {
    let vertex = unsafe { compile_shader(gl, GL_VERTEX_SHADER, vertex_source) }?;
    let fragment = match unsafe { compile_shader(gl, GL_FRAGMENT_SHADER, fragment_source) } {
        Some(shader) => shader,
        None => {
            unsafe {
                (gl.delete_shader)(vertex);
            }
            return None;
        }
    };
    // SAFETY: the current context owns all shader/program objects.
    let program = unsafe { (gl.create_program)() };
    if program == 0 {
        unsafe {
            (gl.delete_shader)(vertex);
            (gl.delete_shader)(fragment);
        }
        return None;
    }
    unsafe {
        (gl.attach_shader)(program, vertex);
        (gl.attach_shader)(program, fragment);
        (gl.link_program)(program);
        (gl.delete_shader)(vertex);
        (gl.delete_shader)(fragment);
    }
    let mut linked = 0;
    unsafe {
        (gl.get_program)(program, GL_LINK_STATUS, &mut linked);
    }
    if linked == 0 {
        unsafe {
            (gl.delete_program)(program);
        }
        return None;
    }
    let mut texture = 0;
    let mut vao = 0;
    unsafe {
        (gl.gen_textures)(1, &mut texture);
        (gl.gen_vertex_arrays)(1, &mut vao);
        if texture == 0 || vao == 0 {
            if texture != 0 {
                (gl.delete_textures)(1, &texture);
            }
            if vao != 0 {
                (gl.delete_vertex_arrays)(1, &vao);
            }
            (gl.delete_program)(program);
            return None;
        }
        (gl.active_texture)(GL_TEXTURE0);
        (gl.bind_texture)(GL_TEXTURE_2D, texture);
        (gl.tex_parameter)(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
        (gl.tex_parameter)(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
        (gl.tex_parameter)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
        (gl.tex_parameter)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
        set_unpack_defaults(gl);
        (gl.tex_image)(
            GL_TEXTURE_2D,
            0,
            GL_RGBA8,
            width as c_int,
            height as c_int,
            0,
            GL_RGBA,
            GL_UNSIGNED_BYTE,
            ptr::null(),
        );
        (gl.use_program)(program);
        let location = (gl.uniform_location)(program, c"overlay_texture".as_ptr());
        if location >= 0 {
            (gl.uniform_1i)(location, 0);
        }
    }
    Some((texture, program, vao))
}

unsafe fn compile_shader(gl: &Gl, kind: c_uint, source: &[u8]) -> Option<c_uint> {
    // SAFETY: source is static NUL-terminated shader text.
    let shader = unsafe { (gl.create_shader)(kind) };
    if shader == 0 {
        return None;
    }
    let pointer = source.as_ptr().cast::<c_char>();
    unsafe {
        (gl.shader_source)(shader, 1, &pointer, ptr::null());
        (gl.compile_shader)(shader);
    }
    let mut compiled = 0;
    unsafe {
        (gl.get_shader)(shader, GL_COMPILE_STATUS, &mut compiled);
    }
    if compiled == 0 {
        unsafe {
            (gl.delete_shader)(shader);
        }
        None
    } else {
        Some(shader)
    }
}

/// Copies `region` of a tightly packed frame into `staging` and leaves it
/// bound as the unpack buffer, returning the upload source (offset 0).
/// Uploading from process memory made NVIDIA serialize each large copy into
/// the game's frame; a GL-owned buffer lets the GPU fetch the rows itself.
/// None leaves no buffer bound, for a direct upload from `bytes`.
///
/// # Safety
/// The renderer's context is current, `staging` is zero or its buffer,
/// `bytes` holds `width`-wide rows, and `region` lies within them.
unsafe fn stage_rows(
    gl: &Gl,
    staging: c_uint,
    bytes: &[u8],
    width: u32,
    region: DamageRect,
) -> Option<*const c_void> {
    if staging == 0 {
        return None;
    }
    let length = region.width as usize * 4;
    let size = length * region.height as usize;
    let stride = width as usize * 4;
    unsafe {
        (gl.bind_buffer)(GL_PIXEL_UNPACK_BUFFER, staging);
        // Fresh storage, so an upload still reading the previous rows is never waited on.
        (gl.buffer_data)(
            GL_PIXEL_UNPACK_BUFFER,
            size as isize,
            ptr::null(),
            GL_STREAM_DRAW,
        );
        let mapped = (gl.map_buffer_range)(
            GL_PIXEL_UNPACK_BUFFER,
            0,
            size as isize,
            GL_MAP_WRITE_BIT | GL_MAP_INVALIDATE_BUFFER_BIT,
        );
        if mapped.is_null() {
            (gl.bind_buffer)(GL_PIXEL_UNPACK_BUFFER, 0);
            return None;
        }
        for row in 0..region.height as usize {
            let offset = (region.y as usize + row) * stride + region.x as usize * 4;
            ptr::copy_nonoverlapping(
                bytes.as_ptr().add(offset),
                mapped.cast::<u8>().add(row * length),
                length,
            );
        }
        // A lost mapping (rare) leaves undefined contents; upload directly instead.
        if (gl.unmap_buffer)(GL_PIXEL_UNPACK_BUFFER) == 0 {
            (gl.bind_buffer)(GL_PIXEL_UNPACK_BUFFER, 0);
            return None;
        }
    }
    Some(ptr::null())
}

unsafe fn set_unpack_defaults(gl: &Gl) {
    // SAFETY: these pixel-store values are valid for tightly packed RGBA8.
    unsafe {
        (gl.bind_buffer)(GL_PIXEL_UNPACK_BUFFER, 0);
        (gl.pixel_store)(GL_UNPACK_ALIGNMENT, 4);
        (gl.pixel_store)(GL_UNPACK_ROW_LENGTH, 0);
        (gl.pixel_store)(GL_UNPACK_SKIP_ROWS, 0);
        (gl.pixel_store)(GL_UNPACK_SKIP_PIXELS, 0);
    }
}

unsafe fn set_enabled(gl: &Gl, capability: c_uint, enabled: bool) {
    // SAFETY: capability is valid for the active GL3/ES3 profile.
    unsafe {
        if capability == GL_BLEND && gl.has_indexed_state {
            if enabled {
                if let Some(function) = gl.enable_i {
                    function(capability, 0);
                }
            } else if let Some(function) = gl.disable_i {
                function(capability, 0);
            }
            return;
        }
        if enabled {
            (gl.enable)(capability);
        } else {
            (gl.disable)(capability);
        }
    }
}

impl Gl {
    unsafe fn blend_function(&self, src_rgb: u32, dst_rgb: u32, src_alpha: u32, dst_alpha: u32) {
        unsafe {
            if self.has_indexed_blend {
                if let Some(function) = self.blend_func_i {
                    function(0, src_rgb, dst_rgb, src_alpha, dst_alpha);
                }
            } else {
                (self.blend_func_separate)(src_rgb, dst_rgb, src_alpha, dst_alpha);
            }
        }
    }
    unsafe fn blend_equation(&self, rgb: u32, alpha: u32) {
        unsafe {
            if self.has_indexed_blend {
                if let Some(function) = self.blend_equation_i {
                    function(0, rgb, alpha);
                }
            } else {
                (self.blend_equation_separate)(rgb, alpha);
            }
        }
    }
    unsafe fn mask(&self, r: u8, g: u8, b: u8, a: u8) {
        unsafe {
            if self.has_indexed_state {
                if let Some(function) = self.color_mask_i {
                    function(0, r, g, b, a);
                }
            } else {
                (self.color_mask)(r, g, b, a);
            }
        }
    }
}
