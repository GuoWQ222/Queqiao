//! The CUDA driver API, the part NVENC needs on Linux (docs/LINUX.md): a context on a GPU, pitched memory, and
//! copies from memory into it. `libcuda.so.1` comes with NVIDIA's driver and is loaded at run time, so Queqiao
//! starts on machines without it.

use std::ffi::{CStr, c_char, c_int, c_uint, c_void};

type CuResult = c_int;
pub(crate) type CuContext = *mut c_void;
pub(crate) type CuDevicePtr = u64;

/// A shared library opened with `dlopen`, closed when dropped.
pub(crate) struct Library(*mut c_void);

impl Library {
    pub(crate) fn open(name: &CStr) -> Result<Library, String> {
        let handle = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if handle.is_null() {
            Err(format!("{} not found", name.to_string_lossy()))
        } else {
            Ok(Library(handle))
        }
    }

    /// The function `name`, as `T` (an `extern "C" fn` type of the same signature).
    ///
    /// # Safety
    /// `T` must be a function pointer type matching the symbol's real signature.
    pub(crate) unsafe fn function<T: Copy>(&self, name: &CStr) -> Result<T, String> {
        assert_eq!(size_of::<T>(), size_of::<*mut c_void>());
        let address = unsafe { libc::dlsym(self.0, name.as_ptr()) };
        if address.is_null() {
            Err(format!("{} missing", name.to_string_lossy()))
        } else {
            Ok(unsafe { std::mem::transmute_copy(&address) })
        }
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        unsafe { libc::dlclose(self.0) };
    }
}

/// CUDA_MEMCPY2D (cuda.h).
#[repr(C)]
struct Memcpy2D {
    src_x_in_bytes: usize,
    src_y: usize,
    src_memory_type: c_uint,
    src_host: *const c_void,
    src_device: CuDevicePtr,
    src_array: *mut c_void,
    src_pitch: usize,
    dst_x_in_bytes: usize,
    dst_y: usize,
    dst_memory_type: c_uint,
    dst_host: *mut c_void,
    dst_device: CuDevicePtr,
    dst_array: *mut c_void,
    dst_pitch: usize,
    width_in_bytes: usize,
    height: usize,
}

const MEMORY_TYPE_HOST: c_uint = 1;
const MEMORY_TYPE_DEVICE: c_uint = 2;

struct Api {
    device_get_name: unsafe extern "C" fn(*mut c_char, c_int, c_int) -> CuResult,
    ctx_create: unsafe extern "C" fn(*mut CuContext, c_uint, c_int) -> CuResult,
    ctx_destroy: unsafe extern "C" fn(CuContext) -> CuResult,
    ctx_push: unsafe extern "C" fn(CuContext) -> CuResult,
    ctx_pop: unsafe extern "C" fn(*mut CuContext) -> CuResult,
    mem_alloc_pitch:
        unsafe extern "C" fn(*mut CuDevicePtr, *mut usize, usize, usize, c_uint) -> CuResult,
    mem_free: unsafe extern "C" fn(CuDevicePtr) -> CuResult,
    memcpy_2d: unsafe extern "C" fn(*const Memcpy2D) -> CuResult,
    get_error_name: unsafe extern "C" fn(CuResult, *mut *const c_char) -> CuResult,
}

/// A CUDA context on one GPU, destroyed when dropped (before the library is closed).
pub(crate) struct Cuda {
    api: Api,
    context: CuContext,
    pub(crate) device_name: String,
    _library: Library,
}

impl Cuda {
    /// A context on GPU `ordinal` (0: the first).
    pub(crate) fn open(ordinal: c_int) -> Result<Cuda, String> {
        let library = Library::open(c"libcuda.so.1")?;
        let (api, init, count, device_get) = unsafe {
            let init: unsafe extern "C" fn(c_uint) -> CuResult = library.function(c"cuInit")?;
            let count: unsafe extern "C" fn(*mut c_int) -> CuResult =
                library.function(c"cuDeviceGetCount")?;
            let device_get: unsafe extern "C" fn(*mut c_int, c_int) -> CuResult =
                library.function(c"cuDeviceGet")?;
            let api = Api {
                device_get_name: library.function(c"cuDeviceGetName")?,
                ctx_create: library.function(c"cuCtxCreate_v2")?,
                ctx_destroy: library.function(c"cuCtxDestroy_v2")?,
                ctx_push: library.function(c"cuCtxPushCurrent_v2")?,
                ctx_pop: library.function(c"cuCtxPopCurrent_v2")?,
                mem_alloc_pitch: library.function(c"cuMemAllocPitch_v2")?,
                mem_free: library.function(c"cuMemFree_v2")?,
                memcpy_2d: library.function(c"cuMemcpy2D_v2")?,
                get_error_name: library.function(c"cuGetErrorName")?,
            };
            (api, init, count, device_get)
        };
        let mut cuda = Cuda {
            api,
            context: std::ptr::null_mut(),
            device_name: String::new(),
            _library: library,
        };
        cuda.check(unsafe { init(0) }, "cuInit")?;
        let mut devices = 0;
        cuda.check(unsafe { count(&mut devices) }, "cuDeviceGetCount")?;
        if ordinal >= devices {
            return Err(format!("no CUDA GPU {ordinal} ({devices} found)"));
        }
        let mut device = 0;
        cuda.check(unsafe { device_get(&mut device, ordinal) }, "cuDeviceGet")?;
        let mut name = [0 as c_char; 128];
        cuda.check(
            unsafe { (cuda.api.device_get_name)(name.as_mut_ptr(), 127, device) },
            "cuDeviceGetName",
        )?;
        cuda.device_name = unsafe { CStr::from_ptr(name.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        let mut context = std::ptr::null_mut();
        cuda.check(
            unsafe { (cuda.api.ctx_create)(&mut context, 0, device) },
            "cuCtxCreate",
        )?;
        cuda.context = context;
        // Created current on this thread; NVENC is given the context itself, copies push it (`current`).
        let mut popped = std::ptr::null_mut();
        cuda.check(
            unsafe { (cuda.api.ctx_pop)(&mut popped) },
            "cuCtxPopCurrent",
        )?;
        Ok(cuda)
    }

    pub(crate) fn context(&self) -> CuContext {
        self.context
    }

    /// GPU memory of `rows` rows of at least `width_bytes` bytes: its address and the rows' pitch.
    pub(crate) fn alloc_pitch(
        &self,
        width_bytes: usize,
        rows: usize,
    ) -> Result<(CuDevicePtr, usize), String> {
        self.current(|| {
            let (mut pointer, mut pitch) = (0, 0);
            let code = unsafe {
                (self.api.mem_alloc_pitch)(&mut pointer, &mut pitch, width_bytes, rows, 16)
            };
            self.check(code, "cuMemAllocPitch")?;
            Ok((pointer, pitch))
        })
    }

    pub(crate) fn free(&self, pointer: CuDevicePtr) {
        let _ = self.current(|| {
            let code = unsafe { (self.api.mem_free)(pointer) };
            self.check(code, "cuMemFree")
        });
    }

    /// Copies `rows` rows of `width_bytes` from memory (`source`, rows `source_pitch` apart) to the GPU at `target`
    /// (rows `target_pitch` apart). Returns when the copy is done.
    pub(crate) fn upload(
        &self,
        source: &[u8],
        source_pitch: usize,
        target: CuDevicePtr,
        target_pitch: usize,
        width_bytes: usize,
        rows: usize,
    ) -> Result<(), String> {
        if rows == 0 {
            return Ok(());
        }
        let needed = source_pitch * (rows - 1) + width_bytes;
        if source.len() < needed || width_bytes > source_pitch.min(target_pitch) {
            return Err(format!(
                "upload of {rows} rows of {width_bytes} bytes from {} bytes",
                source.len()
            ));
        }
        let copy = Memcpy2D {
            src_x_in_bytes: 0,
            src_y: 0,
            src_memory_type: MEMORY_TYPE_HOST,
            src_host: source.as_ptr().cast(),
            src_device: 0,
            src_array: std::ptr::null_mut(),
            src_pitch: source_pitch,
            dst_x_in_bytes: 0,
            dst_y: 0,
            dst_memory_type: MEMORY_TYPE_DEVICE,
            dst_host: std::ptr::null_mut(),
            dst_device: target,
            dst_array: std::ptr::null_mut(),
            dst_pitch: target_pitch,
            width_in_bytes: width_bytes,
            height: rows,
        };
        self.current(|| {
            let code = unsafe { (self.api.memcpy_2d)(&copy) };
            self.check(code, "cuMemcpy2D")
        })
    }

    /// Runs `work` with the context current on this thread.
    fn current<T>(&self, work: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        self.check(
            unsafe { (self.api.ctx_push)(self.context) },
            "cuCtxPushCurrent",
        )?;
        let result = work();
        let mut popped = std::ptr::null_mut();
        let _ = unsafe { (self.api.ctx_pop)(&mut popped) };
        result
    }

    fn check(&self, code: CuResult, step: &str) -> Result<(), String> {
        if code == 0 {
            return Ok(());
        }
        let mut name: *const c_char = std::ptr::null();
        let _ = unsafe { (self.api.get_error_name)(code, &mut name) };
        let name = if name.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(name) }
                .to_string_lossy()
                .into_owned()
        };
        Err(format!("CUDA {step}: error {code} {name}"))
    }
}

impl Drop for Cuda {
    fn drop(&mut self) {
        if !self.context.is_null() {
            unsafe { (self.api.ctx_destroy)(self.context) };
        }
    }
}
