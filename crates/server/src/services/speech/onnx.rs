//! The few ONNX Runtime calls Kokoro needs, through its C API, with the
//! library loaded at runtime: the app doesn't link ONNX Runtime, and only
//! those who use text to speech download it. Types come from `ort-sys`; the
//! API version asked for is its minimum (17), which every newer runtime
//! still serves.
//!
//! Not the `ort` crate (its Windows exit hook crashes debug builds before
//! `main`), nor `libloading`: its `raw-dylib` imports of `kernel32.dll`
//! functions the C runtime also imports make the MSVC linker drop the C
//! runtime's `KERNEL32.dll` import, and the app crashes at start. The three
//! calls needed are declared below through `kernel32.lib` instead.

use std::ffi::{CStr, CString, c_char, c_void};
use std::path::Path;
use std::ptr;

use anyhow::{Context, anyhow};
use ort_sys as sys;

/// A loaded library, freed when dropped.
struct Library(*mut c_void);

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn LoadLibraryExW(name: *const u16, file: *mut c_void, flags: u32) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
    fn FreeLibrary(module: *mut c_void) -> i32;
    fn GetLastError() -> u32;
}

#[cfg(windows)]
impl Library {
    fn open(path: &Path) -> anyhow::Result<Library> {
        /// Looks for the library's own dependencies next to it.
        const LOAD_WITH_ALTERED_SEARCH_PATH: u32 = 0x8;
        let name = os_path(path);
        // SAFETY: a NUL-terminated wide path; loading runs the library's
        // initialisation, as any program using it does.
        let module = unsafe { LoadLibraryExW(name.as_ptr(), ptr::null_mut(), LOAD_WITH_ALTERED_SEARCH_PATH) };
        if module.is_null() {
            // SAFETY: no other call in between.
            let code = unsafe { GetLastError() };
            return Err(anyhow!("couldn't load {} (Windows error {code})", path.display()));
        }
        Ok(Library(module))
    }

    fn symbol(&self, name: &CStr) -> *mut c_void {
        // SAFETY: a loaded module and a NUL-terminated name.
        unsafe { GetProcAddress(self.0, name.as_ptr()) }
    }
}

#[cfg(windows)]
impl Drop for Library {
    fn drop(&mut self) {
        // SAFETY: loaded by `open`, freed once.
        unsafe { FreeLibrary(self.0) };
    }
}

#[cfg(not(windows))]
impl Library {
    fn open(path: &Path) -> anyhow::Result<Library> {
        Err(anyhow!("text to speech only runs on Windows so far ({})", path.display()))
    }

    fn symbol(&self, _name: &CStr) -> *mut c_void {
        ptr::null_mut()
    }
}

/// One model loaded in ONNX Runtime, on the CPU.
pub struct Model {
    api: *const sys::OrtApi,
    env: *mut sys::OrtEnv,
    session: *mut sys::OrtSession,
    memory: *mut sys::OrtMemoryInfo,
    output: CString,
    /// Last, so it's unloaded after everything above is released.
    _library: Library,
}

// SAFETY: ONNX Runtime's environment, session and memory info aren't tied
// to the thread that made them; `run` takes `&mut self`, so calls never
// overlap.
unsafe impl Send for Model {}

/// An input tensor: its name, shape and data.
pub struct Input<'a> {
    pub name: &'a CStr,
    pub shape: &'a [i64],
    pub data: InputData<'a>,
}

pub enum InputData<'a> {
    I64(&'a mut [i64]),
    F32(&'a mut [f32]),
}

impl Model {
    /// Loads ONNX Runtime from `library`, then the model, with `threads`
    /// threads per run.
    pub fn load(library: &Path, model_path: &Path, threads: usize) -> anyhow::Result<Model> {
        let library = Library::open(library)
            .context("couldn't load ONNX Runtime (try installing text to speech again)")?;
        let get_base = library.symbol(c"OrtGetApiBase");
        if get_base.is_null() {
            return Err(anyhow!("that library isn't ONNX Runtime"));
        }
        // SAFETY: `OrtGetApiBase` has this signature in every ONNX Runtime,
        // and the base it returns lives as long as the library.
        let api = unsafe {
            let get_base: unsafe extern "system" fn() -> *const sys::OrtApiBase =
                std::mem::transmute(get_base);
            let base = get_base();
            if base.is_null() {
                return Err(anyhow!("ONNX Runtime gave no API"));
            }
            ((*base).GetApi)(sys::ORT_API_VERSION)
        };
        if api.is_null() {
            return Err(anyhow!("this ONNX Runtime is too old (API {} needed)", sys::ORT_API_VERSION));
        }
        let mut model = Model {
            api,
            env: ptr::null_mut(),
            session: ptr::null_mut(),
            memory: ptr::null_mut(),
            output: CString::default(),
            _library: library,
        };
        // SAFETY: `api` is the runtime's function table, valid while the
        // library is loaded; out-pointers are fields of `model`, whose
        // `Drop` releases whatever was created, also after an error.
        unsafe {
            let api = &*model.api;
            let status = (api.CreateEnv)(
                sys::OrtLoggingLevel::ORT_LOGGING_LEVEL_WARNING,
                c"lsk".as_ptr(),
                &mut model.env,
            );
            model.check(status)?;
            let mut options: *mut sys::OrtSessionOptions = ptr::null_mut();
            model.check((api.CreateSessionOptions)(&mut options))?;
            let status = (api.SetIntraOpNumThreads)(options, threads.clamp(1, 64) as i32);
            let mut created = model.check(status);
            if created.is_ok() {
                let path = os_path(model_path);
                let status = (api.CreateSession)(model.env, path.as_ptr(), options, &mut model.session);
                created = model.check(status);
            }
            (api.ReleaseSessionOptions)(options);
            created.with_context(|| format!("couldn't load the model {}", model_path.display()))?;
            let status = (api.CreateCpuMemoryInfo)(
                sys::OrtAllocatorType::OrtArenaAllocator,
                sys::OrtMemType::OrtMemTypeDefault,
                &mut model.memory,
            );
            model.check(status)?;
            model.output = model.output_name()?;
        }
        Ok(model)
    }

    fn api(&self) -> &sys::OrtApi {
        // SAFETY: set in `load` from the loaded library, which outlives `self`.
        unsafe { &*self.api }
    }

    /// An error for a failed call; the status is released.
    fn check(&self, status: sys::OrtStatusPtr) -> anyhow::Result<()> {
        if status.0.is_null() {
            return Ok(());
        }
        // SAFETY: a non-null status from the runtime, released once here.
        unsafe {
            let message =
                CStr::from_ptr((self.api().GetErrorMessage)(status.0)).to_string_lossy().into_owned();
            (self.api().ReleaseStatus)(status.0);
            Err(anyhow!(message))
        }
    }

    /// An error unless the model takes these inputs.
    pub fn expect_inputs(&self, wanted: &[&str]) -> anyhow::Result<()> {
        let names = self.input_names()?;
        if wanted.iter().all(|name| names.iter().any(|have| have == name)) {
            Ok(())
        } else {
            Err(anyhow!("the model takes {names:?}, not {wanted:?}: is it the right file?"))
        }
    }

    /// The model's input names, in order.
    pub fn input_names(&self) -> anyhow::Result<Vec<String>> {
        // SAFETY: as in `output_name`.
        unsafe {
            let api = self.api();
            let mut count = 0usize;
            self.check((api.SessionGetInputCount)(self.session, &mut count))?;
            let mut allocator: *mut sys::OrtAllocator = ptr::null_mut();
            self.check((api.GetAllocatorWithDefaultOptions)(&mut allocator))?;
            let mut names = Vec::with_capacity(count);
            for index in 0..count {
                let mut name: *mut c_char = ptr::null_mut();
                self.check((api.SessionGetInputName)(self.session, index, allocator, &mut name))?;
                names.push(CStr::from_ptr(name).to_string_lossy().into_owned());
                self.check((api.AllocatorFree)(allocator, name.cast()))?;
            }
            Ok(names)
        }
    }

    /// The model's first output's name.
    fn output_name(&self) -> anyhow::Result<CString> {
        // SAFETY: the session is loaded; the name is copied, then freed with
        // the allocator that made it.
        unsafe {
            let api = self.api();
            let mut allocator: *mut sys::OrtAllocator = ptr::null_mut();
            self.check((api.GetAllocatorWithDefaultOptions)(&mut allocator))?;
            let mut name: *mut c_char = ptr::null_mut();
            self.check((api.SessionGetOutputName)(self.session, 0, allocator, &mut name))?;
            let copy = CStr::from_ptr(name).to_owned();
            self.check((api.AllocatorFree)(allocator, name.cast()))?;
            Ok(copy)
        }
    }

    /// Runs the model; returns its first output as floats.
    pub fn run(&mut self, inputs: &mut [Input<'_>]) -> anyhow::Result<Vec<f32>> {
        let mut values: Vec<*mut sys::OrtValue> = Vec::with_capacity(inputs.len());
        let result = self.run_with(inputs, &mut values);
        for value in values {
            // SAFETY: created by `CreateTensorWithDataAsOrtValue` below.
            unsafe { (self.api().ReleaseValue)(value) };
        }
        result
    }

    fn run_with(
        &mut self,
        inputs: &mut [Input<'_>],
        values: &mut Vec<*mut sys::OrtValue>,
    ) -> anyhow::Result<Vec<f32>> {
        // SAFETY: each tensor borrows its input's data, which outlives the
        // run (the values are released by `run` right after); the output is
        // copied out before it's released.
        unsafe {
            let api = self.api();
            for input in inputs.iter_mut() {
                let (data, bytes, kind): (*mut c_void, usize, _) = match &mut input.data {
                    InputData::I64(data) => (
                        data.as_mut_ptr().cast(),
                        std::mem::size_of_val(&**data),
                        sys::ONNXTensorElementDataType::ONNX_TENSOR_ELEMENT_DATA_TYPE_INT64,
                    ),
                    InputData::F32(data) => (
                        data.as_mut_ptr().cast(),
                        std::mem::size_of_val(&**data),
                        sys::ONNXTensorElementDataType::ONNX_TENSOR_ELEMENT_DATA_TYPE_FLOAT,
                    ),
                };
                let mut value: *mut sys::OrtValue = ptr::null_mut();
                self.check((api.CreateTensorWithDataAsOrtValue)(
                    self.memory,
                    data,
                    bytes,
                    input.shape.as_ptr(),
                    input.shape.len(),
                    kind,
                    &mut value,
                ))?;
                values.push(value);
            }
            let names: Vec<*const c_char> = inputs.iter().map(|input| input.name.as_ptr()).collect();
            let constant: Vec<*const sys::OrtValue> = values.iter().map(|value| value.cast_const()).collect();
            let output_names = [self.output.as_ptr()];
            let mut output: *mut sys::OrtValue = ptr::null_mut();
            self.check((api.Run)(
                self.session,
                ptr::null(),
                names.as_ptr(),
                constant.as_ptr(),
                constant.len(),
                output_names.as_ptr(),
                1,
                &mut output,
            ))?;
            let copied = self.floats(output);
            (api.ReleaseValue)(output);
            copied
        }
    }

    /// A float tensor's data.
    ///
    /// # Safety
    /// `value` must be a live tensor value.
    unsafe fn floats(&self, value: *mut sys::OrtValue) -> anyhow::Result<Vec<f32>> {
        unsafe {
            let api = self.api();
            let mut info: *mut sys::OrtTensorTypeAndShapeInfo = ptr::null_mut();
            self.check((api.GetTensorTypeAndShape)(value, &mut info))?;
            let mut count = 0usize;
            let counted = self.check((api.GetTensorShapeElementCount)(info, &mut count));
            (api.ReleaseTensorTypeAndShapeInfo)(info);
            counted?;
            let mut data: *mut c_void = ptr::null_mut();
            self.check((api.GetTensorMutableData)(value, &mut data))?;
            if count == 0 || data.is_null() {
                return Ok(Vec::new());
            }
            Ok(std::slice::from_raw_parts(data.cast::<f32>(), count).to_vec())
        }
    }
}

impl Drop for Model {
    fn drop(&mut self) {
        // SAFETY: each was created in `load` (or is null) and is released once.
        unsafe {
            let api = &*self.api;
            if !self.memory.is_null() {
                (api.ReleaseMemoryInfo)(self.memory);
            }
            if !self.session.is_null() {
                (api.ReleaseSession)(self.session);
            }
            if !self.env.is_null() {
                (api.ReleaseEnv)(self.env);
            }
        }
    }
}

/// A path as ONNX Runtime takes it: UTF-16 on Windows, bytes elsewhere,
/// NUL-terminated.
#[cfg(windows)]
fn os_path(path: &Path) -> Vec<sys::os_char> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

#[cfg(not(windows))]
fn os_path(path: &Path) -> Vec<sys::os_char> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().iter().map(|byte| *byte as sys::os_char).chain(std::iter::once(0)).collect()
}
