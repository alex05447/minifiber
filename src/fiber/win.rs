use {
    crate::FiberError,
    std::{ffi::c_void, io},
    winapi::{
        shared::minwindef::{DWORD, LPVOID},
        um::{
            fibersapi::IsThreadAFiber,
            winbase::{ConvertThreadToFiberEx, CreateFiberEx, DeleteFiber, SwitchToFiber},
        },
    },
};

/// WinAPI / MSVC `GetCurrentFiber` macro / intrinsic implemented in `.asm`, built by `build.rs`.
extern "C" {
    fn get_current_fiber() -> LPVOID;
}

const FIBER_FLAG_FLOAT_SWITCH: DWORD = 0x1;

/// Wrapper around a fiber / green thread / stackful coroutine.
/// Releases the OS object handle / deallocates the stack when dropped.
/// It's up to the user to ensure all resources used by the fiber have been freed.
pub struct Fiber {
    // Cleaned up by the `Drop` handler.
    fiber: LPVOID,
    // Need to keep track of fibers created from threads to NOT `DeleteFiber` them in the `Drop` handler.
    from_thread: bool,
    name: Option<String>,
    // This is here to be dropped when the `Fiber` is dropped,
    // freeing the `entry_point` memory and all its owned resources.
    //
    // Only used by `Fibers` created via `new`.
    _entry_point: Option<Box<dyn FiberEntryPoint>>,
}

unsafe impl Send for Fiber {}

/// User-provided [`Fiber`] entry point as a closure.
/// Used for [`Fiber::new`].
///
/// [`Fiber`]: struct.Fiber.html
/// [`Fiber::new`]: struct.Fiber.html#method.new
pub trait FiberEntryPoint: FnOnce() + 'static {}

impl<F: FnOnce() + 'static> FiberEntryPoint for F {}

/// User-provided [`Fiber`] entry point as a raw unsafe function.
/// Used for [`Fiber::new_fn`].
///
/// [`Fiber`]: struct.Fiber.html
/// [`Fiber::new_fn`]: struct.Fiber.html#method.new_fn
pub type FiberEntryPointFn = unsafe extern "system" fn(*mut c_void);

impl Fiber {
    /// Creates a new fiber with the specified `stack_size`, `name` and [`entry_point`].
    ///
    /// The fiber does not run until it is switched to via [`switch_to`]
    /// by a thread converted to a fiber via [`from_thread`].
    ///
    /// # Errors
    ///
    /// Returns an error if `stack_size` is `0`.
    /// Returns an error if the OS function fails.
    ///
    /// # Safety
    ///
    /// The `Fiber` boxes and holds on to its entry point closure / function, and holds on to the OS fiber object handle.
    /// When the `Fiber` object is dropped normally, this boxed entry point memory and all its owned resource,
    /// as well as the OS fiber object handle, are freed.
    ///
    /// However, the boxed entry point memory and the OS fiber object handle are also freed if the `Fiber` exits its entry point
    /// (thus also exiting / terminating the thread running it).
    /// `Drop`'ping such a `Fiber` in a different thread leads to a double free event.
    ///
    /// It is recommended to never allow a `Fiber` created by [`new`] to exit its entry point - e.g. by
    /// switching back to a "thread" fiber, created by [`from_thread`]. This way any fibers created by [`new`]
    /// may the be explicitly `Drop`'ped, freeing the OS fiber objects and cleaning up the entry point memory safely.
    ///
    /// [`entry_point`]: trait.FiberEntryPoint.html
    /// [`switch_to`]: #method.switch_to
    /// [`from_thread`]: #method.from_thread
    /// [`new`]: #method.new
    pub fn new<N, F>(stack_size: usize, name: N, entry_point: F) -> Result<Fiber, FiberError>
    where
        N: Into<Option<String>>,
        F: FiberEntryPoint,
    {
        let entry_point: Box<dyn FiberEntryPoint> = Box::new(entry_point);

        let mut fiber = unsafe {
            Self::new_fn(
                stack_size,
                name,
                Fiber::fiber_entry_point::<F>,
                entry_point.as_ref() as *const _ as _,
            )
        }?;

        fiber._entry_point.replace(entry_point);

        Ok(fiber)
    }

    /// Creates a new fiber with the specified `stack_size`, `name` and raw entry point function.
    /// Provided `arg` (which may be null) is passed to the entry point.
    ///
    /// The fiber does not run until it is switched to via [`switch_to`]
    /// by a thread converted to a fiber via [`from_thread`].
    ///
    /// # Errors
    ///
    /// Returns an error if `stack_size` is `0`.
    /// Returns an error if the OS function fails.
    ///
    /// # Safety
    ///
    /// It's entirely up to the user to ensure the `entry_point` uses the `arg` correctly / safely,
    /// as well as cleans up any owned resources, including those remaining on the fiber's stack
    /// after the last switch from this `Fiber`, if any.
    ///
    /// The `Fiber` holds on to the OS fiber object handle.
    /// When the `Fiber` object is dropped normally, the OS fiber object handle is freed.
    ///
    /// However, the OS fiber object handle must also be freed via [`free_fiber`] as the last call in the entry point
    /// when the `Fiber` would otherwise exit its entry point (thus also exiting / terminating the thread running it).
    /// `Drop`'ping a `Fiber` which called [`free_fiber`] in a different thread leads to a double free event.
    ///
    /// It is recommended to never allow a `Fiber` created by [`new_fn`] to exit its entry point - e.g. by
    /// switching back to a "thread" fiber, created by [`from_thread`]. This way any fibers created by [`new_fn`]
    /// may the be explicitly `Drop`'ped, freeing the OS fiber objects safely.
    ///
    /// [`switch_to`]: #method.switch_to
    /// [`from_thread`]: #method.from_thread
    /// [`free_fiber`]: #method.free_fiber
    /// [`new_fn`]: #method.new_fn
    pub unsafe fn new_fn<N>(
        stack_size: usize,
        name: N,
        entry_point: FiberEntryPointFn,
        arg: *mut c_void,
    ) -> Result<Fiber, FiberError>
    where
        N: Into<Option<String>>,
    {
        use FiberError::*;

        if stack_size == 0 {
            return Err(InvalidStackSize);
        }

        let fiber = CreateFiberEx(
            stack_size as _,
            stack_size as _,
            FIBER_FLAG_FLOAT_SWITCH,
            Some(entry_point),
            arg,
        );

        if fiber.is_null() {
            Err(FailedToCreate(io::Error::last_os_error()))
        } else {
            Ok(Fiber {
                fiber,
                from_thread: false,
                name: name.into(),
                _entry_point: None,
            })
        }
    }

    /// Converts the current thread to a fiber with the specified `name`.
    ///
    /// Stack size is determined by the calling thread stack size.
    /// This allows it to switch to other fibers created via [`new`].
    ///
    /// # Errors
    ///
    /// Returns an error if the OS function fails.
    ///
    /// [`new`]: #method.new
    pub fn from_thread<N: Into<Option<String>>>(name: N) -> Result<Fiber, FiberError> {
        let current_fiber = Fiber::get_current_fiber();

        // Current thread is already a fiber - we assume that's OK.
        let fiber = if let Some(current_fiber) = current_fiber {
            current_fiber
        } else {
            unsafe { ConvertThreadToFiberEx(0 as LPVOID, FIBER_FLAG_FLOAT_SWITCH) }
        };

        if fiber.is_null() {
            Err(FiberError::FailedToCreate(io::Error::last_os_error()))
        } else {
            Ok(Fiber {
                fiber,
                from_thread: true,
                name: name.into(),
                _entry_point: None,
            })
        }
    }

    /// Switches the current thread's execution context to that of this fiber.
    /// Thread must have previously called [`from_thread`].
    ///
    /// [`from_thread`]: #method.from_thread
    pub fn switch_to(&self) {
        debug_assert!(!self.fiber.is_null());

        unsafe { SwitchToFiber(self.fiber) };
    }

    /// Returns the fiber's name, if any was provided on creation via [`new`] / [`from_thread`].
    ///
    /// [`new`]: #method.new
    /// [`from_thread`]: #method.from_thread
    pub fn name(&self) -> Option<&str> {
        self.name.as_ref().map(|s| s.as_str())
    }

    /// Returns `true` if this fiber was created from a thread.
    pub fn is_thread_fiber(&self) -> bool {
        self.from_thread
    }

    /// Frees the OS fiber object and exits / terminates the current thread running it.
    ///
    /// This must be the last call in the entry point of the `Fiber` created via [`new_fn`]
    /// when the `Fiber` would otherwise exit its entry point (thus also exiting / terminating the thread running it).
    ///
    /// # Safety
    ///
    /// See [`new_fn`] - `Drop`'ping a `Fiber` which called [`free_fiber`] in a different thread leads to a double free event.
    ///
    /// [`new_fn`]: #method.new_fn
    /// [`free_fiber`]: #method.free_fiber
    pub unsafe fn free_fiber() {
        if let Some(fiber) = Self::get_current_fiber() {
            DeleteFiber(fiber)
        }
    }

    fn is_thread_a_fiber() -> bool {
        unsafe { IsThreadAFiber() > 0 }
    }

    fn get_current_fiber() -> Option<LPVOID> {
        if Self::is_thread_a_fiber() {
            Some(unsafe { get_current_fiber() })
        } else {
            None
        }
    }

    extern "system" fn fiber_entry_point<F>(entry_point: *mut c_void)
    where
        F: FiberEntryPoint,
    {
        debug_assert!(!entry_point.is_null());
        let entry_point: Box<F> = unsafe { Box::from_raw(entry_point as _) };

        entry_point();

        // `entry_point` memory has been freed if we reached this point.
        // Free the OS fiber handle as well - otherwise the thread will just exit, leaking the fiber stack.
        //
        // Quote from https://docs.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-deletefiber:
        // "... If the currently running fiber calls DeleteFiber, its thread calls ExitThread and terminates. "

        unsafe { Fiber::free_fiber() };
    }
}

impl Drop for Fiber {
    fn drop(&mut self) {
        // Do not delete the fiber if created by `from_thread()`.
        if !self.from_thread {
            unsafe {
                DeleteFiber(self.fiber);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        minithreadlocal::ThreadLocal,
        std::{
            sync::{
                atomic::{AtomicUsize, Ordering},
                Arc,
            },
            thread,
        },
    };

    #[test]
    fn basic() {
        static mut NUM_RESOURCES: u32 = 0;

        struct Resource {}

        impl Resource {
            fn new() -> Self {
                unsafe {
                    NUM_RESOURCES += 1;
                }

                Self {}
            }

            fn foo(&self) {
                println!("Hello from resource.");
            }

            fn num_resources() -> u32 {
                unsafe { NUM_RESOURCES }
            }
        }

        impl Drop for Resource {
            fn drop(&mut self) {
                unsafe {
                    debug_assert!(NUM_RESOURCES > 0);
                    NUM_RESOURCES -= 1;
                }
            }
        }

        assert_eq!(Resource::num_resources(), 0);

        // Fiber to switch to.
        let mut switch_back_to_fiber: ThreadLocal<Fiber> = ThreadLocal::new().unwrap();

        // Convert the current thread to a fiber.
        let main_fiber = Fiber::from_thread("Main fiber".to_owned()).unwrap();

        // Create a couple of other fibers.
        let worker_fiber_1_arg = Arc::new(AtomicUsize::new(0));
        let worker_fiber_1_arg_clone = worker_fiber_1_arg.clone();

        let worker_fiber_1_resource = Resource::new();

        let switch_back_to_fiber_for_worker_fiber_1 = switch_back_to_fiber.clone();
        let worker_fiber_1 = Fiber::new(64 * 1024, "Worker fiber 1".to_owned(), move || {
            // Do some work.
            worker_fiber_1_arg_clone.fetch_add(1, Ordering::SeqCst);

            worker_fiber_1_resource.foo();

            // Switch back to worker thread.
            assert_eq!(
                unsafe { switch_back_to_fiber_for_worker_fiber_1.as_ref_unchecked() }
                    .name()
                    .unwrap(),
                "Thread 1 fiber"
            );
            unsafe { switch_back_to_fiber_for_worker_fiber_1.as_ref_unchecked() }.switch_to();
        })
        .unwrap();

        assert_eq!(Resource::num_resources(), 1);

        let worker_fiber_2_arg = Arc::new(AtomicUsize::new(0));
        let worker_fiber_2_arg_clone = worker_fiber_2_arg.clone();

        let worker_fiber_2_resource = Resource::new();

        let switch_back_to_fiber_for_worker_fiber_2 = switch_back_to_fiber.clone();
        let worker_fiber_2 = Fiber::new(64 * 1024, "Worker fiber 2".to_owned(), move || {
            // Do some work.
            worker_fiber_2_arg_clone.fetch_add(2, Ordering::SeqCst);

            worker_fiber_2_resource.foo();

            // Switch back to main thread.
            assert_eq!(
                unsafe { switch_back_to_fiber_for_worker_fiber_2.as_ref_unchecked() }
                    .name()
                    .unwrap(),
                "Main fiber"
            );
            unsafe { switch_back_to_fiber_for_worker_fiber_2.as_ref_unchecked() }.switch_to();
        })
        .unwrap();

        assert_eq!(Resource::num_resources(), 2);

        // Create a thread which will run a worker fiber.
        let switch_back_to_fiber_for_thread_1 = switch_back_to_fiber.clone();
        let thread_1 = thread::spawn(move || {
            // Convert to a fiber first.
            let fiber = Fiber::from_thread("Thread 1 fiber".to_owned()).unwrap();

            // Store the fiber to the TLS so that the worker fiber can switch back.
            switch_back_to_fiber_for_thread_1.store(fiber).unwrap();

            // Switch to `worker_fiber_1`, which will execute it.
            worker_fiber_1.switch_to();
            // The fiber has executed and switched back.

            // Ensure the work has completed.
            assert_eq!(worker_fiber_1_arg.load(Ordering::SeqCst), 1);

            // Drop the fiber - this should clean up the fiber entry point memory
            // and the owned resources.
            std::mem::drop(worker_fiber_1);

            // Clean up TLS.
            switch_back_to_fiber_for_thread_1.take().unwrap();
        });

        // Run the other worker fiber in the main thread.

        // Store the fiber to the TLS so that the worker fiber can switch back.
        switch_back_to_fiber.store(main_fiber).unwrap();

        // Switch to `worker_fiber_2`, which will execute it.
        worker_fiber_2.switch_to();
        // The fiber has executed and switched back.

        // Ensure the work has completed.
        assert_eq!(worker_fiber_2_arg.load(Ordering::SeqCst), 2);

        // Wait for the thread.
        thread_1.join().unwrap();

        // Drop the fiber - this should clean up the fiber entry point memory
        // and the owned resources.
        std::mem::drop(worker_fiber_2);

        assert_eq!(Resource::num_resources(), 0);

        // Clean up TLS.
        switch_back_to_fiber.take().unwrap();
        switch_back_to_fiber.free_index().unwrap();
    }
}
