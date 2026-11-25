//! Reading a framebuffer in a GPU's own tiled layout (KMS capture, the service's desktop helper): the buffer, exported
//! as DMA-BUFs (one per plane, with its modifier), imported as an EGL image, drawn by the GPU into a plain texture and
//! read back as B, G, R, A. That is how a tiled layout (Intel's X/Y tiling and compression, AMD's, NVIDIA's block
//! linear) becomes pixels without knowing it here. libEGL and libGLESv2 are the system's (Mesa's or NVIDIA's), loaded at
//! run time; the EGL display is the GPU of the DRM card itself (EGL_EXT_platform_device), else the default one.

use std::ffi::{CStr, CString, c_char, c_void};
use std::os::fd::RawFd;

type Display = *mut c_void;
type Context = *mut c_void;
type Image = *mut c_void;
type Device = *mut c_void;

const EGL_NONE: i32 = 0x3038;
const EGL_EXTENSIONS: i32 = 0x3055;
const EGL_OPENGL_ES_API: u32 = 0x30A0;
const EGL_CONTEXT_CLIENT_VERSION: i32 = 0x3098;
const EGL_PLATFORM_DEVICE_EXT: u32 = 0x313F;
const EGL_PLATFORM_SURFACELESS_MESA: u32 = 0x31DD;
const EGL_DRM_DEVICE_FILE_EXT: i32 = 0x3233;
const EGL_LINUX_DMA_BUF_EXT: u32 = 0x3270;
const EGL_WIDTH: i32 = 0x3057;
const EGL_HEIGHT: i32 = 0x3056;
const EGL_LINUX_DRM_FOURCC_EXT: i32 = 0x3271;
/// Per plane: FD, OFFSET, PITCH, MODIFIER_LO, MODIFIER_HI.
const PLANE_ATTRIBUTES: [[i32; 5]; 4] = [
    [0x3272, 0x3273, 0x3274, 0x3443, 0x3444],
    [0x3275, 0x3276, 0x3277, 0x3445, 0x3446],
    [0x3278, 0x3279, 0x327A, 0x3447, 0x3448],
    [0x3440, 0x3441, 0x3442, 0x3449, 0x344A],
];

const GL_TEXTURE_2D: u32 = 0x0DE1;
const GL_TEXTURE_EXTERNAL_OES: u32 = 0x8D65;
const GL_TEXTURE_MIN_FILTER: u32 = 0x2801;
const GL_TEXTURE_MAG_FILTER: u32 = 0x2800;
const GL_TEXTURE_WRAP_S: u32 = 0x2802;
const GL_TEXTURE_WRAP_T: u32 = 0x2803;
const GL_NEAREST: i32 = 0x2600;
const GL_CLAMP_TO_EDGE: i32 = 0x812F;
const GL_RGBA: u32 = 0x1908;
const GL_UNSIGNED_BYTE: u32 = 0x1401;
const GL_FRAMEBUFFER: u32 = 0x8D40;
const GL_COLOR_ATTACHMENT0: u32 = 0x8CE0;
const GL_FRAMEBUFFER_COMPLETE: u32 = 0x8CD5;
const GL_VERTEX_SHADER: u32 = 0x8B31;
const GL_FRAGMENT_SHADER: u32 = 0x8B30;
const GL_COMPILE_STATUS: u32 = 0x8B81;
const GL_LINK_STATUS: u32 = 0x8B82;
const GL_FLOAT: u32 = 0x1406;
const GL_TRIANGLE_STRIP: u32 = 0x0005;
const GL_TEXTURE0: u32 = 0x84C0;
const GL_PACK_ALIGNMENT: u32 = 0x0D05;

/// Loads one function, by name, from a library or through eglGetProcAddress.
macro_rules! functions {
    ($name:ident { $($field:ident : fn($($arg:ty),*) $(-> $ret:ty)? ;)* }) => {
        #[allow(non_snake_case)]
        struct $name {
            $($field: unsafe extern "C" fn($($arg),*) $(-> $ret)?,)*
        }
        impl $name {
            fn load(find: &dyn Fn(&str) -> *mut c_void) -> Result<$name, String> {
                Ok($name {
                    $($field: {
                        let found = find(stringify!($field));
                        if found.is_null() {
                            return Err(format!("{} is not there", stringify!($field)));
                        }
                        unsafe { std::mem::transmute::<*mut c_void, unsafe extern "C" fn($($arg),*) $(-> $ret)?>(found) }
                    },)*
                })
            }
        }
    };
}

functions!(Egl {
    eglGetProcAddress: fn(*const c_char) -> *mut c_void;
    eglInitialize: fn(Display, *mut i32, *mut i32) -> u32;
    eglTerminate: fn(Display) -> u32;
    eglBindAPI: fn(u32) -> u32;
    eglCreateContext: fn(Display, *mut c_void, Context, *const i32) -> Context;
    eglDestroyContext: fn(Display, Context) -> u32;
    eglMakeCurrent: fn(Display, *mut c_void, *mut c_void, Context) -> u32;
    eglGetError: fn() -> i32;
    eglQueryString: fn(Display, i32) -> *const c_char;
});

functions!(EglExtensions {
    eglGetPlatformDisplayEXT: fn(u32, *mut c_void, *const i32) -> Display;
    eglCreateImageKHR: fn(Display, Context, u32, *mut c_void, *const i32) -> Image;
    eglDestroyImageKHR: fn(Display, Image) -> u32;
    glEGLImageTargetTexture2DOES: fn(u32, Image);
});

functions!(Devices {
    eglQueryDevicesEXT: fn(i32, *mut Device, *mut i32) -> u32;
    eglQueryDeviceStringEXT: fn(Device, i32) -> *const c_char;
});

functions!(Gl {
    glGetError: fn() -> u32;
    glGenTextures: fn(i32, *mut u32);
    glDeleteTextures: fn(i32, *const u32);
    glBindTexture: fn(u32, u32);
    glTexParameteri: fn(u32, u32, i32);
    glTexImage2D: fn(u32, i32, i32, i32, i32, i32, u32, u32, *const c_void);
    glActiveTexture: fn(u32);
    glGenFramebuffers: fn(i32, *mut u32);
    glDeleteFramebuffers: fn(i32, *const u32);
    glBindFramebuffer: fn(u32, u32);
    glFramebufferTexture2D: fn(u32, u32, u32, u32, i32);
    glCheckFramebufferStatus: fn(u32) -> u32;
    glViewport: fn(i32, i32, i32, i32);
    glCreateShader: fn(u32) -> u32;
    glShaderSource: fn(u32, i32, *const *const c_char, *const i32);
    glCompileShader: fn(u32);
    glGetShaderiv: fn(u32, u32, *mut i32);
    glCreateProgram: fn() -> u32;
    glAttachShader: fn(u32, u32);
    glBindAttribLocation: fn(u32, u32, *const c_char);
    glLinkProgram: fn(u32);
    glGetProgramiv: fn(u32, u32, *mut i32);
    glUseProgram: fn(u32);
    glGetUniformLocation: fn(u32, *const c_char) -> i32;
    glUniform1i: fn(i32, i32);
    glVertexAttribPointer: fn(u32, i32, u32, u8, i32, *const c_void);
    glEnableVertexAttribArray: fn(u32);
    glDrawArrays: fn(u32, i32, i32);
    glReadPixels: fn(i32, i32, i32, i32, u32, u32, *mut c_void);
    glPixelStorei: fn(u32, i32);
    glFinish: fn();
});

fn open(name: &CStr) -> Result<*mut c_void, String> {
    let library = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    (!library.is_null())
        .then_some(library)
        .ok_or_else(|| format!("{} is not on this system", name.to_string_lossy()))
}

fn symbol(library: *mut c_void, name: &str) -> *mut c_void {
    let Ok(name) = CString::new(name) else {
        return std::ptr::null_mut();
    };
    unsafe { libc::dlsym(library, name.as_ptr()) }
}

const VERTEX: &str = "attribute vec2 position;\nvarying vec2 uv;\nvoid main() {\n    uv = position * 0.5 + 0.5;\n    gl_Position = vec4(position, 0.0, 1.0);\n}\n";
const PRECISION: &str = "#ifdef GL_FRAGMENT_PRECISION_HIGH\nprecision highp float;\n#else\nprecision mediump float;\n#endif\n";
/// The picture's red, green and blue go out as blue, green, red: read back as RGBA, the bytes are B, G, R, A.
const FRAGMENT_BODY: &str = "varying vec2 uv;\nvoid main() {\n    gl_FragColor = vec4(texture2D(picture, uv).bgr, 1.0);\n}\n";

/// One plane of a framebuffer, as a DMA-BUF.
pub(crate) struct Plane {
    pub fd: RawFd,
    pub offset: u32,
    pub pitch: u32,
}

/// The GPU's context for reading framebuffers (current on the thread that made it, which reads with it).
pub(crate) struct EglReader {
    egl: Egl,
    extensions: EglExtensions,
    gl: Gl,
    display: Display,
    context: Context,
    /// The programs for a plain texture and for an external one (some layouts are imported only as those).
    programs: [u32; 2],
    pub description: String,
}

impl EglReader {
    /// On the GPU of `card` (`/dev/dri/cardN`), else EGL's default display.
    pub(crate) fn new(card: Option<&str>) -> Result<EglReader, String> {
        let egl_library = open(c"libEGL.so.1")?;
        let gl_library = open(c"libGLESv2.so.2")?;
        let egl = Egl::load(&|name| symbol(egl_library, name))?;
        let proc_address = |name: &str| {
            let Ok(c_name) = CString::new(name) else {
                return std::ptr::null_mut();
            };
            unsafe { (egl.eglGetProcAddress)(c_name.as_ptr()) }
        };
        let extensions = EglExtensions::load(&proc_address)?;
        let (display, description) = Self::display(&egl, &extensions, &proc_address, card)?;
        let (mut major, mut minor) = (0, 0);
        if unsafe { (egl.eglInitialize)(display, &mut major, &mut minor) } == 0 {
            return Err(format!("eglInitialize: {:#x}", unsafe {
                (egl.eglGetError)()
            }));
        }
        let names = unsafe { (egl.eglQueryString)(display, EGL_EXTENSIONS) };
        let names = if names.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(names) }
                .to_string_lossy()
                .into_owned()
        };
        for needed in [
            "EGL_EXT_image_dma_buf_import",
            "EGL_KHR_surfaceless_context",
        ] {
            if !names.split(' ').any(|n| n == needed) {
                unsafe { (egl.eglTerminate)(display) };
                return Err(format!("{description}: no {needed}"));
            }
        }
        unsafe { (egl.eglBindAPI)(EGL_OPENGL_ES_API) };
        let attributes = [EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE];
        // EGL_KHR_no_config_context: no config is needed for a context that draws into textures only.
        let context = unsafe {
            (egl.eglCreateContext)(
                display,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                attributes.as_ptr(),
            )
        };
        if context.is_null()
            || unsafe {
                (egl.eglMakeCurrent)(display, std::ptr::null_mut(), std::ptr::null_mut(), context)
            } == 0
        {
            let error = unsafe { (egl.eglGetError)() };
            unsafe { (egl.eglTerminate)(display) };
            return Err(format!("{description}: no context ({error:#x})"));
        }
        let gl = Gl::load(&|name| {
            let found = symbol(gl_library, name);
            if found.is_null() {
                proc_address(name)
            } else {
                found
            }
        })?;
        let mut reader = EglReader {
            egl,
            extensions,
            gl,
            display,
            context,
            programs: [0, 0],
            description: format!("{description}, EGL {major}.{minor}"),
        };
        reader.programs[0] = reader.program(&format!(
            "{PRECISION}uniform sampler2D picture;\n{FRAGMENT_BODY}"
        ))?;
        // External textures need an extension; without it only plain ones are read.
        reader.programs[1] = reader
            .program(&format!(
                "#extension GL_OES_EGL_image_external : require\n{PRECISION}uniform samplerExternalOES picture;\n{FRAGMENT_BODY}"
            ))
            .unwrap_or(0);
        Ok(reader)
    }

    fn display(
        egl: &Egl,
        extensions: &EglExtensions,
        proc_address: &dyn Fn(&str) -> *mut c_void,
        card: Option<&str>,
    ) -> Result<(Display, String), String> {
        // The device whose DRM file is the card's.
        if let (Some(card), Ok(devices)) = (card, Devices::load(proc_address)) {
            let mut list = [std::ptr::null_mut(); 16];
            let mut count = 0;
            if unsafe {
                (devices.eglQueryDevicesEXT)(list.len() as i32, list.as_mut_ptr(), &mut count)
            } != 0
            {
                for &device in &list[..count.clamp(0, 16) as usize] {
                    let file = unsafe {
                        (devices.eglQueryDeviceStringEXT)(device, EGL_DRM_DEVICE_FILE_EXT)
                    };
                    if !file.is_null() && unsafe { CStr::from_ptr(file) }.to_str() == Ok(card) {
                        let display = unsafe {
                            (extensions.eglGetPlatformDisplayEXT)(
                                EGL_PLATFORM_DEVICE_EXT,
                                device,
                                [EGL_NONE].as_ptr(),
                            )
                        };
                        if !display.is_null() {
                            return Ok((display, format!("the EGL device of {card}")));
                        }
                    }
                }
            }
        }
        let _ = egl;
        let display = unsafe {
            (extensions.eglGetPlatformDisplayEXT)(
                EGL_PLATFORM_SURFACELESS_MESA,
                std::ptr::null_mut(),
                [EGL_NONE].as_ptr(),
            )
        };
        if !display.is_null() {
            return Ok((display, "EGL's default GPU (surfaceless)".into()));
        }
        // The first device, whatever its card.
        if let Ok(devices) = Devices::load(proc_address) {
            let mut device = std::ptr::null_mut();
            let mut count = 0;
            if unsafe { (devices.eglQueryDevicesEXT)(1, &mut device, &mut count) } != 0 && count > 0
            {
                let display = unsafe {
                    (extensions.eglGetPlatformDisplayEXT)(
                        EGL_PLATFORM_DEVICE_EXT,
                        device,
                        [EGL_NONE].as_ptr(),
                    )
                };
                if !display.is_null() {
                    return Ok((display, "EGL's first device".into()));
                }
            }
        }
        Err("no EGL display for the GPU".into())
    }

    fn program(&self, fragment: &str) -> Result<u32, String> {
        let gl = &self.gl;
        let shader = |kind: u32, source: &str| -> Result<u32, String> {
            let source = CString::new(source).map_err(|_| "shader text")?;
            unsafe {
                let shader = (gl.glCreateShader)(kind);
                let pointer = source.as_ptr();
                (gl.glShaderSource)(shader, 1, &pointer, std::ptr::null());
                (gl.glCompileShader)(shader);
                let mut compiled = 0;
                (gl.glGetShaderiv)(shader, GL_COMPILE_STATUS, &mut compiled);
                if compiled == 0 {
                    return Err("a shader did not compile".into());
                }
                Ok(shader)
            }
        };
        let (vertex, fragment) = (
            shader(GL_VERTEX_SHADER, VERTEX)?,
            shader(GL_FRAGMENT_SHADER, fragment)?,
        );
        unsafe {
            let program = (gl.glCreateProgram)();
            (gl.glAttachShader)(program, vertex);
            (gl.glAttachShader)(program, fragment);
            (gl.glBindAttribLocation)(program, 0, c"position".as_ptr());
            (gl.glLinkProgram)(program);
            let mut linked = 0;
            (gl.glGetProgramiv)(program, GL_LINK_STATUS, &mut linked);
            if linked == 0 {
                return Err("the program did not link".into());
            }
            Ok(program)
        }
    }

    /// The area `(left, top, width, height)` of a `size` framebuffer of `fourcc` in the layout `modifier` (None: the
    /// driver's implicit one), as B, G, R, A rows from the top.
    pub(crate) fn read(
        &mut self,
        planes: &[Plane],
        size: (u32, u32),
        fourcc: u32,
        modifier: Option<u64>,
        area: (u32, u32, u32, u32),
    ) -> Result<Vec<u8>, String> {
        let current = unsafe {
            (self.egl.eglMakeCurrent)(
                self.display,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                self.context,
            )
        };
        if current == 0 {
            return Err("the EGL context is not current".into());
        }
        let mut attributes = vec![
            EGL_WIDTH,
            size.0 as i32,
            EGL_HEIGHT,
            size.1 as i32,
            EGL_LINUX_DRM_FOURCC_EXT,
            fourcc as i32,
        ];
        for (plane, names) in planes.iter().zip(PLANE_ATTRIBUTES) {
            attributes.extend([
                names[0],
                plane.fd,
                names[1],
                plane.offset as i32,
                names[2],
                plane.pitch as i32,
            ]);
            if let Some(modifier) = modifier {
                attributes.extend([
                    names[3],
                    modifier as u32 as i32,
                    names[4],
                    (modifier >> 32) as u32 as i32,
                ]);
            }
        }
        attributes.push(EGL_NONE);
        let image = unsafe {
            (self.extensions.eglCreateImageKHR)(
                self.display,
                std::ptr::null_mut(),
                EGL_LINUX_DMA_BUF_EXT,
                std::ptr::null_mut(),
                attributes.as_ptr(),
            )
        };
        if image.is_null() {
            return Err(format!(
                "the GPU did not take the framebuffer ({:#x}; modifier {modifier:?})",
                unsafe { (self.egl.eglGetError)() }
            ));
        }
        let result = self.draw_and_read(image, area);
        unsafe { (self.extensions.eglDestroyImageKHR)(self.display, image) };
        result
    }

    fn draw_and_read(
        &mut self,
        image: Image,
        area: (u32, u32, u32, u32),
    ) -> Result<Vec<u8>, String> {
        let gl = &self.gl;
        let (left, top, width, height) = area;
        unsafe {
            while (gl.glGetError)() != 0 {}
            // The imported picture: a plain texture where the layout allows, else an external one.
            let mut source = 0;
            (gl.glGenTextures)(1, &mut source);
            let mut target = GL_TEXTURE_2D;
            (gl.glActiveTexture)(GL_TEXTURE0);
            (gl.glBindTexture)(target, source);
            (self.extensions.glEGLImageTargetTexture2DOES)(target, image);
            if (gl.glGetError)() != 0 && self.programs[1] != 0 {
                (gl.glDeleteTextures)(1, &source);
                (gl.glGenTextures)(1, &mut source);
                target = GL_TEXTURE_EXTERNAL_OES;
                (gl.glBindTexture)(target, source);
                (self.extensions.glEGLImageTargetTexture2DOES)(target, image);
            }
            if (gl.glGetError)() != 0 {
                (gl.glDeleteTextures)(1, &source);
                return Err("the GPU could not make a texture of the framebuffer".into());
            }
            for (name, value) in [
                (GL_TEXTURE_MIN_FILTER, GL_NEAREST),
                (GL_TEXTURE_MAG_FILTER, GL_NEAREST),
                (GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE),
                (GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE),
            ] {
                (gl.glTexParameteri)(target, name, value);
            }
            // The plain texture it is drawn into, as large as the framebuffer: row 0 at the bottom of the drawing is the
            // framebuffer's first row, so reading from row `top` up gives the rows from the top.
            let (full_width, full_height) = (left + width, top + height);
            let mut drawn = 0;
            (gl.glGenTextures)(1, &mut drawn);
            (gl.glBindTexture)(GL_TEXTURE_2D, drawn);
            (gl.glTexImage2D)(
                GL_TEXTURE_2D,
                0,
                GL_RGBA as i32,
                full_width as i32,
                full_height as i32,
                0,
                GL_RGBA,
                GL_UNSIGNED_BYTE,
                std::ptr::null(),
            );
            let mut framebuffer = 0;
            (gl.glGenFramebuffers)(1, &mut framebuffer);
            (gl.glBindFramebuffer)(GL_FRAMEBUFFER, framebuffer);
            (gl.glFramebufferTexture2D)(
                GL_FRAMEBUFFER,
                GL_COLOR_ATTACHMENT0,
                GL_TEXTURE_2D,
                drawn,
                0,
            );
            let result = if (gl.glCheckFramebufferStatus)(GL_FRAMEBUFFER) != GL_FRAMEBUFFER_COMPLETE
            {
                Err("the GPU cannot draw into a plain texture of that size".to_string())
            } else {
                let program = self.programs[usize::from(target == GL_TEXTURE_EXTERNAL_OES)];
                (gl.glViewport)(0, 0, full_width as i32, full_height as i32);
                (gl.glUseProgram)(program);
                (gl.glActiveTexture)(GL_TEXTURE0);
                (gl.glBindTexture)(target, source);
                (gl.glUniform1i)((gl.glGetUniformLocation)(program, c"picture".as_ptr()), 0);
                let corners: [f32; 8] = [-1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0];
                (gl.glVertexAttribPointer)(0, 2, GL_FLOAT, 0, 0, corners.as_ptr().cast());
                (gl.glEnableVertexAttribArray)(0);
                (gl.glDrawArrays)(GL_TRIANGLE_STRIP, 0, 4);
                let mut pixels = vec![0u8; width as usize * height as usize * 4];
                (gl.glPixelStorei)(GL_PACK_ALIGNMENT, 1);
                (gl.glReadPixels)(
                    left as i32,
                    top as i32,
                    width as i32,
                    height as i32,
                    GL_RGBA,
                    GL_UNSIGNED_BYTE,
                    pixels.as_mut_ptr().cast(),
                );
                (gl.glFinish)();
                match (gl.glGetError)() {
                    0 => Ok(pixels),
                    error => Err(format!("drawing the framebuffer: GL error {error:#x}")),
                }
            };
            (gl.glBindFramebuffer)(GL_FRAMEBUFFER, 0);
            (gl.glDeleteFramebuffers)(1, &framebuffer);
            (gl.glDeleteTextures)(1, &drawn);
            (gl.glDeleteTextures)(1, &source);
            result
        }
    }
}

impl Drop for EglReader {
    fn drop(&mut self) {
        unsafe {
            (self.egl.eglMakeCurrent)(
                self.display,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            (self.egl.eglDestroyContext)(self.display, self.context);
            (self.egl.eglTerminate)(self.display);
        }
    }
}

functions!(Export {
    eglExportDMABUFImageQueryMESA: fn(Display, Image, *mut i32, *mut i32, *mut u64) -> u32;
    eglExportDMABUFImageMESA: fn(Display, Image, *mut i32, *mut i32, *mut i32) -> u32;
});

const EGL_GL_TEXTURE_2D_KHR: u32 = 0x30B1;

/// Tests: a picture put by the GPU into a texture of its own layout, exported as DMA-BUFs (EGL_MESA_image_dma_buf_export)
/// and read back through `read`, as a tiled framebuffer is; compared pixel by pixel. What it found, or what differed.
pub fn self_test(card: Option<&str>) -> Result<String, String> {
    let mut reader = EglReader::new(card)?;
    let export = Export::load(&|name| {
        let Ok(c_name) = CString::new(name) else {
            return std::ptr::null_mut();
        };
        unsafe { (reader.egl.eglGetProcAddress)(c_name.as_ptr()) }
    })
    .map_err(|e| format!("{}: no export of DMA-BUFs ({e})", reader.description))?;
    let (width, height) = (640u32, 360u32);
    let pattern: Vec<u8> = (0..height)
        .flat_map(|y| (0..width).flat_map(move |x| [x as u8, y as u8, (x ^ y) as u8 | 0x10, 255]))
        .collect();
    let gl = &reader.gl;
    let mut texture = 0;
    unsafe {
        (gl.glGenTextures)(1, &mut texture);
        (gl.glBindTexture)(GL_TEXTURE_2D, texture);
        (gl.glTexImage2D)(
            GL_TEXTURE_2D,
            0,
            GL_RGBA as i32,
            width as i32,
            height as i32,
            0,
            GL_RGBA,
            GL_UNSIGNED_BYTE,
            pattern.as_ptr().cast(),
        );
        // Complete without mipmaps, or EGL makes no image a driver will export.
        (gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
        (gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
        (gl.glFinish)();
    }
    let image = unsafe {
        (reader.extensions.eglCreateImageKHR)(
            reader.display,
            reader.context,
            EGL_GL_TEXTURE_2D_KHR,
            texture as usize as *mut c_void,
            [EGL_NONE].as_ptr(),
        )
    };
    if image.is_null() {
        return Err(format!("no EGL image of a texture ({:#x})", unsafe {
            (reader.egl.eglGetError)()
        }));
    }
    let (mut fourcc, mut count, mut modifiers) = (0i32, 0i32, [0u64; 4]);
    let (mut fds, mut strides, mut offsets) = ([-1i32; 4], [0i32; 4], [0i32; 4]);
    let queried = unsafe {
        (export.eglExportDMABUFImageQueryMESA)(
            reader.display,
            image,
            &mut fourcc,
            &mut count,
            modifiers.as_mut_ptr(),
        )
    } != 0;
    let exported = queried
        && unsafe {
            (export.eglExportDMABUFImageMESA)(
                reader.display,
                image,
                fds.as_mut_ptr(),
                strides.as_mut_ptr(),
                offsets.as_mut_ptr(),
            )
        } != 0;
    unsafe { (reader.extensions.eglDestroyImageKHR)(reader.display, image) };
    if !exported {
        let names = unsafe { (reader.egl.eglQueryString)(reader.display, EGL_EXTENSIONS) };
        let listed = !names.is_null()
            && unsafe { CStr::from_ptr(names) }
                .to_string_lossy()
                .split(' ')
                .any(|n| n == "EGL_MESA_image_dma_buf_export");
        return Err(format!(
            "{}: the texture was not exported ({} failed: {:#x}; queried {:?}, {count} planes, modifier {:#x}; EGL_MESA_image_dma_buf_export {}; NVIDIA exports only with nvidia-drm's modeset on)",
            reader.description,
            if queried { "the export" } else { "the query" },
            unsafe { (reader.egl.eglGetError)() },
            String::from_utf8_lossy(&(fourcc as u32).to_le_bytes()),
            modifiers[0],
            if listed { "listed" } else { "not listed" }
        ));
    }
    let count = count.clamp(1, 4) as usize;
    let planes: Vec<Plane> = (0..count)
        .map(|i| Plane {
            fd: if fds[i] >= 0 { fds[i] } else { fds[0] },
            offset: offsets[i] as u32,
            pitch: strides[i] as u32,
        })
        .collect();
    let result = reader.read(
        &planes,
        (width, height),
        fourcc as u32,
        Some(modifiers[0]),
        (0, 0, width, height),
    );
    for fd in fds.iter().filter(|fd| **fd >= 0) {
        unsafe { libc::close(*fd) };
    }
    unsafe { (reader.gl.glDeleteTextures)(1, &texture) };
    let pixels = result?;
    let wrong = pixels
        .as_chunks::<4>()
        .0
        .iter()
        .zip(pattern.as_chunks::<4>().0)
        .filter(|(got, sent)| got[..3] != [sent[2], sent[1], sent[0]])
        .count();
    let layout = format!(
        "{}: {} planes of {:?}, modifier {:#x}",
        reader.description,
        count,
        String::from_utf8_lossy(&(fourcc as u32).to_le_bytes()),
        modifiers[0]
    );
    if wrong > 0 {
        return Err(format!(
            "{layout}: {wrong} of {} pixels read wrong",
            width * height
        ));
    }
    Ok(format!(
        "{layout}: all {} pixels read right",
        width * height
    ))
}
