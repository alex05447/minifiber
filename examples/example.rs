use minifiber::*;

fn main() {
    use {
        minithreadlocal::ThreadLocal,
        std::{
            sync::{
                atomic::{AtomicUsize, Ordering},
                Arc,
            },
            thread,
        },
    };

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
