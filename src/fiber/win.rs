use {
    crate::FiberError,
    std::{ffi::c_void, io},
    winapi::{
        shared::minwindef::{DWORD, LPVOID},
        um::{
            fibersapi::IsThreadAFiber,
            winbase::{
                ConvertFiberToThread, ConvertThreadToFiberEx, CreateFiberEx, DeleteFiber,
                SwitchToFiber,
            },
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
/// `T` is custom, arbitrary per-fiber state.
pub struct Fiber<T> {
    // Cleaned up by the `Drop` handler.
    fiber: LPVOID,
    // Need to keep track of fibers created from threads to NOT `DeleteFiber` them in the `Drop` handler,
    // but instead to `ConvertFiberToThread`.
    from_thread: bool,
    // This is here to be dropped when the `Fiber` is dropped,
    // freeing the `entry_point` memory and all its owned resources.
    //
    // Only used by `Fibers` created via `new`.
    _entry_point: Option<Box<dyn FiberEntryPoint>>,
    // Custom per-fiber state.
    state: T,
}

unsafe impl<T: Send> Send for Fiber<T> {}

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

impl<T> Fiber<T> {
    /// Creates a new fiber with the specified `stack_size`, `state` and [`entry_point`].
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
    pub fn new<F: FiberEntryPoint>(stack_size: usize, state: T, entry_point: F) -> Result<Self, FiberError> {
        let entry_point: Box<dyn FiberEntryPoint> = Box::new(entry_point);

        let mut fiber = unsafe {
            Self::new_fn(
                stack_size,
                state,
                Self::fiber_entry_point::<F>,
                entry_point.as_ref() as *const _ as _,
            )
        }?;

        fiber._entry_point.replace(entry_point);

        Ok(fiber)
    }

    /// Creates a new fiber with the specified `stack_size`, `state` and raw entry point function.
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
    pub unsafe fn new_fn(
        stack_size: usize,
        state: T,
        entry_point: FiberEntryPointFn,
        arg: *mut c_void,
    ) -> Result<Self, FiberError> {
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
                _entry_point: None,
                state,
            })
        }
    }

    /// Converts the current thread to a fiber with the specified `state`.
    /// This allows it to [`switch_to`] other fibers created via [`new`] / [`new_fn`].
    ///
    /// Stack size is determined by the calling thread stack size.
    ///
    /// # Errors
    ///
    /// Returns an error if the OS function fails,
    /// or if the current thread was already converted to a fiber via a previous call
    /// to this method and has not been converted back to a normal, non-fiber thread
    /// by dropping the returned fiber object.
    ///
    /// # Safety
    ///
    /// When the returned `Fiber` is `Drop`'ped, its thread is converted back to a normal thread.
    /// Hence the user must never `Send` such a fiber to another thread.
    ///
    /// [`new`]: #method.new
    /// [`new_fn`]: #method.new_fn
    /// [`switch_to`]: #method.switch_to
    pub fn from_thread(state: T) -> Result<Self, FiberError> {
        if Self::get_current_fiber().is_some() {
            return Err(FiberError::ThreadAlreadyAFiber);
        }

        let fiber = unsafe { ConvertThreadToFiberEx(0 as LPVOID, FIBER_FLAG_FLOAT_SWITCH) };

        if fiber.is_null() {
            Err(FiberError::FailedToCreate(io::Error::last_os_error()))
        } else {
            Ok(Fiber {
                fiber,
                from_thread: true,
                _entry_point: None,
                state
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

    /// Returns the reference to the per-fiber state, which was provided to it on creation via [`new`] / [`from_thread`].
    ///
    /// [`new`]: #method.new
    /// [`from_thread`]: #method.from_thread
    pub fn state(&self) -> &T {
        &self.state
    }

    /// Returns the mutable reference to the per-fiber state, which was provided to it on creation via [`new`] / [`from_thread`].
    ///
    /// [`new`]: #method.new
    /// [`from_thread`]: #method.from_thread
    pub fn state_mut(&mut self) -> &mut T {
        &mut self.state
    }

    /// Returns `true` if this fiber was [`created from a thread`].
    ///
    /// [`created from a thread`]: #method.from_thread
    pub fn is_thread_fiber(&self) -> bool {
        self.from_thread
    }

    /// Frees the current thread's OS fiber object and exits / terminates the current thread running it.
    ///
    /// This must be the last call in the entry point of the `Fiber` created via [`new_fn`]
    /// when the `Fiber` would otherwise exit its entry point (thus also exiting / terminating the thread running it).
    /// This function should not be used otherwise.
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

    /// Returns `true` if the current thread has been converted to a fiber via [`from_thread`].
    ///
    /// [`from_thread`]: #method.from_thread
    pub fn is_thread_a_fiber() -> bool {
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

        unsafe { Self::free_fiber() };
    }
}

impl<T> Drop for Fiber<T> {
    fn drop(&mut self) {
        if self.from_thread {
            unsafe {
                ConvertFiberToThread();
            }
        } else {
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

        type NamedFiber = Fiber<String>;

        assert_eq!(Resource::num_resources(), 0);

        // Fiber to switch to.
        let mut switch_back_to_fiber: ThreadLocal<NamedFiber> = ThreadLocal::new().unwrap();

        // Convert the current thread to a fiber.
        assert!(!NamedFiber::is_thread_a_fiber());
        let main_fiber = NamedFiber::from_thread("Main fiber".to_owned()).unwrap();
        assert!(main_fiber.is_thread_fiber());
        assert!(NamedFiber::is_thread_a_fiber());

        assert!(matches!(
            Fiber::from_thread("".to_string()).err().unwrap(),
            FiberError::ThreadAlreadyAFiber
        ));

        // Create a couple of other fibers.
        let worker_fiber_1_arg = Arc::new(AtomicUsize::new(0));
        let worker_fiber_1_arg_clone = worker_fiber_1_arg.clone();

        let worker_fiber_1_resource = Resource::new();

        let switch_back_to_fiber_for_worker_fiber_1 = switch_back_to_fiber.clone();
        let worker_fiber_1 = Fiber::new(64 * 1024, "Worker fiber 1".to_owned(), move || {
            assert!(NamedFiber::is_thread_a_fiber());

            // Do some work.
            worker_fiber_1_arg_clone.fetch_add(1, Ordering::SeqCst);

            worker_fiber_1_resource.foo();

            // Switch back to worker thread.
            assert_eq!(
                unsafe { switch_back_to_fiber_for_worker_fiber_1.as_ref_unchecked() }
                    .state(),
                "Thread 1 fiber"
            );
            unsafe { switch_back_to_fiber_for_worker_fiber_1.as_ref_unchecked() }.switch_to();
        })
        .unwrap();

        assert!(!worker_fiber_1.is_thread_fiber());

        assert_eq!(Resource::num_resources(), 1);

        let worker_fiber_2_arg = Arc::new(AtomicUsize::new(0));
        let worker_fiber_2_arg_clone = worker_fiber_2_arg.clone();

        let worker_fiber_2_resource = Resource::new();

        let switch_back_to_fiber_for_worker_fiber_2 = switch_back_to_fiber.clone();
        let worker_fiber_2 = Fiber::new(64 * 1024, "Worker fiber 2".to_owned(), move || {
            assert!(NamedFiber::is_thread_a_fiber());

            // Do some work.
            worker_fiber_2_arg_clone.fetch_add(2, Ordering::SeqCst);

            worker_fiber_2_resource.foo();

            // Switch back to main thread.
            assert_eq!(
                unsafe { switch_back_to_fiber_for_worker_fiber_2.as_ref_unchecked() }
                    .state(),
                "Main fiber"
            );
            unsafe { switch_back_to_fiber_for_worker_fiber_2.as_ref_unchecked() }.switch_to();
        })
        .unwrap();

        assert!(!worker_fiber_2.is_thread_fiber());

        assert_eq!(Resource::num_resources(), 2);

        // Create a thread which will run a worker fiber.
        let switch_back_to_fiber_for_thread_1 = switch_back_to_fiber.clone();
        let thread_1 = thread::spawn(move || {
            // Convert to a fiber first.
            assert!(!NamedFiber::is_thread_a_fiber());
            let fiber = Fiber::from_thread("Thread 1 fiber".to_owned()).unwrap();
            assert!(fiber.is_thread_fiber());
            assert!(NamedFiber::is_thread_a_fiber());

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
            // Drop the thread fiber and convert the thread back to a normal thread.
            let fiber = switch_back_to_fiber_for_thread_1.take().unwrap().unwrap();
            assert!(fiber.is_thread_fiber());
            std::mem::drop(fiber);
            assert!(!NamedFiber::is_thread_a_fiber());
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
        let main_fiber = switch_back_to_fiber.take().unwrap().unwrap();
        switch_back_to_fiber.free_index().unwrap();

        // Drop the main fiber and convert the main thread back to a normal thread.
        assert!(main_fiber.is_thread_fiber());
        std::mem::drop(main_fiber);
        assert!(!NamedFiber::is_thread_a_fiber());
    }
}
