use {
    std::{ffi::c_void, mem, io},
    crate::FiberError,
    winapi::{shared::minwindef::{DWORD, LPVOID}, um::{fibersapi::IsThreadAFiber, winbase::{ConvertThreadToFiberEx, CreateFiberEx, DeleteFiber, SwitchToFiber}}},
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
    fiber: LPVOID,
    // Need to keep track of fibers created from threads to NOT `DeleteFiber` them.
    from_thread: bool,
    name: Option<String>,
}

unsafe impl Send for Fiber {}
unsafe impl Sync for Fiber {}

impl Fiber {
    /// Creates a new fiber with the specified `stack_size`, `name` and entry point.
    ///
    /// The fiber does not run until it is switched to via [`switch_to`]
    /// by a thread converted to a fiber via [`from_thread`].
    ///
    /// # Errors
    ///
    /// Returns an error if `stack_size` is `0`.
    /// Returns an error if the OS function fails.
    ///
    /// [`switch_to`]: #method.switch_to
    /// [`from_thread`]: #method.from_thread
    pub fn new<'n, N, F>(stack_size: usize, name: N, entry_point: F) -> Result<Fiber, FiberError>
    where
        N: Into<Option<&'n str>>,
        F: FnOnce() + 'static,
    {
        use FiberError::*;

        if stack_size == 0 {
            return Err(InvalidStackSize);
        }

        let entry_point = Box::new(entry_point);

        let fiber = unsafe {
            CreateFiberEx(
                stack_size as _,
                stack_size as _,
                FIBER_FLAG_FLOAT_SWITCH,
                Some(Fiber::fiber_entry_point::<F>),
                Box::into_raw(entry_point) as _,
            )
        };

        if fiber.is_null() {
            Err(FailedToCreate(io::Error::last_os_error()))
        } else {
            Ok(Fiber {
                fiber,
                from_thread: false,
                name: name.into().map(|s| String::from(s)),
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
    pub fn from_thread(name: Option<&str>) -> Result<Fiber, FiberError> {
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
                name: name.map(|s| String::from(s)),
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

    fn is_thread_a_fiber() -> bool {
        unsafe { IsThreadAFiber() > 0 }
    }

    fn get_current_fiber() -> Option<LPVOID> {
        if Fiber::is_thread_a_fiber() {
            Some(unsafe { get_current_fiber() })
        } else {
            None
        }
    }

    extern "system" fn fiber_entry_point<F>(entry_point: *mut c_void)
    where
        F: FnOnce() + 'static,
    {
        assert!(!entry_point.is_null());

        let entry_point: Box<F> = unsafe { Box::from_raw(mem::transmute(entry_point)) };

        entry_point();
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
        std::{
            sync::{
                atomic::{AtomicUsize, Ordering},
                Arc,
            },
            thread,
        },
        super::*,
        minithreadlocal::ThreadLocal
    };

    #[test]
    fn basic() {
        // Fiber to switch to.
        let mut switch_back_to_fiber: ThreadLocal<Fiber> = ThreadLocal::new();

        // Convert the current thread to a fiber.
        let main_fiber = Fiber::from_thread(Some("Main fiber")).unwrap();

        // Create a couple of other fibers.
        let worker_fiber_1_arg = Arc::new(AtomicUsize::new(0));
        let worker_fiber_1_arg_clone = worker_fiber_1_arg.clone();

        let switch_back_to_fiber_for_worker_fiber_1 = switch_back_to_fiber.clone();
        let worker_fiber_1 = Fiber::new(64 * 1024, Some("Worker fiber 1"), move || {
            // Do some work.
            worker_fiber_1_arg_clone.fetch_add(1, Ordering::SeqCst);

            // Switch back to worker thread.
            assert_eq!(
                switch_back_to_fiber_for_worker_fiber_1
                    .as_ref()
                    .name()
                    .unwrap(),
                "Thread 1 fiber"
            );
            switch_back_to_fiber_for_worker_fiber_1.as_ref().switch_to();
        }).unwrap();

        let worker_fiber_2_arg = Arc::new(AtomicUsize::new(0));
        let worker_fiber_2_arg_clone = worker_fiber_2_arg.clone();

        let switch_back_to_fiber_for_worker_fiber_2 = switch_back_to_fiber.clone();
        let worker_fiber_2 = Fiber::new(64 * 1024, Some("Worker fiber 2"), move || {
            // Do some work.
            worker_fiber_2_arg_clone.fetch_add(2, Ordering::SeqCst);

            // Switch back to main thread.
            assert_eq!(
                switch_back_to_fiber_for_worker_fiber_2
                    .as_ref()
                    .name()
                    .unwrap(),
                "Main fiber"
            );
            switch_back_to_fiber_for_worker_fiber_2.as_ref().switch_to();
        }).unwrap();

        // Create a thread which will run a worker fiber.
        let switch_back_to_fiber_for_thread_1 = switch_back_to_fiber.clone();
        let thread_1 = thread::spawn(move || {
            // Convert to a fiber first.
            let fiber = Fiber::from_thread(Some("Thread 1 fiber")).unwrap();

            // Store the fiber to the TLS so that the worker fiber can switch back.
            switch_back_to_fiber_for_thread_1.store(fiber);

            // Switch to `worker_fiber_1`, which will execute it.
            worker_fiber_1.switch_to();
            // The fiber has executed and switched back.

            // Ensure the work has completed.
            assert_eq!(worker_fiber_1_arg.load(Ordering::SeqCst), 1);

            // Clean up TLS.
            switch_back_to_fiber_for_thread_1.take();
        });

        // Run the other worker fiber in the main thread.

        // Store the fiber to the TLS so that the worker fiber can switch back.
        switch_back_to_fiber.store(main_fiber);

        // Switch to `worker_fiber_2`, which will execute it.
        worker_fiber_2.switch_to();
        // The fiber has executed and switched back.

        // Ensure the work has completed.
        assert_eq!(worker_fiber_2_arg.load(Ordering::SeqCst), 2);

        // Wait for the thread.
        thread_1.join().unwrap();

        // Clean up TLS.
        switch_back_to_fiber.take();
        switch_back_to_fiber.free_index();
    }
}
