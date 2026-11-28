//! Background work on a thread pool, timers on their own calloop thread, and main-thread work
//! queued for GPUI's looper, which an eventfd wakes.

use crate::bridge;
use calloop::{EventLoop, channel, timer::TimeoutAction};
use gpui::{
    PlatformDispatcher, Priority, PriorityQueueReceiver, PriorityQueueSender, RunnableVariant,
    profiler,
};
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::Arc;
use std::{mem::MaybeUninit, thread, time::Duration};

struct TimerAfter {
    duration: Duration,
    runnable: RunnableVariant,
}

pub(crate) struct AndroidDispatcher {
    main_sender: PriorityQueueSender<RunnableVariant>,
    main_wake: Arc<OwnedFd>,
    timer_sender: channel::Sender<TimerAfter>,
    background_sender: PriorityQueueSender<RunnableVariant>,
    _background_threads: Vec<thread::JoinHandle<()>>,
    main_thread_id: thread::ThreadId,
}

const MIN_THREADS: usize = 2;

pub(crate) fn run(runnable: RunnableVariant) {
    let location = runnable.metadata().location;
    let spawned = runnable.metadata().spawned;
    profiler::update_running_task(spawned, location);
    runnable.run();
    profiler::save_task_timing();
}

impl AndroidDispatcher {
    pub fn new(
        main_sender: PriorityQueueSender<RunnableVariant>,
        main_wake: Arc<OwnedFd>,
    ) -> Self {
        let (background_sender, background_receiver) = PriorityQueueReceiver::new();
        let thread_count =
            thread::available_parallelism().map_or(MIN_THREADS, |n| n.get().max(MIN_THREADS));
        let mut background_threads = (0..thread_count)
            .map(|i| {
                let receiver: PriorityQueueReceiver<RunnableVariant> = background_receiver.clone();
                thread::Builder::new()
                    .name(format!("Worker-{i}"))
                    .spawn(move || {
                        for runnable in receiver.iter() {
                            run(runnable);
                        }
                    })
                    .expect("spawn a GPUI worker")
            })
            .collect::<Vec<_>>();

        let (timer_sender, timer_channel) = channel::channel::<TimerAfter>();
        let timer_thread = thread::Builder::new()
            .name("Timer".to_owned())
            .spawn(move || {
                let mut event_loop: EventLoop<()> =
                    EventLoop::try_new().expect("Failed to initialize timer loop!");
                let handle = event_loop.handle();
                let timer_handle = event_loop.handle();
                let signal = event_loop.get_signal();
                handle
                    .insert_source(timer_channel, move |event, _, _| match event {
                        channel::Event::Closed => signal.stop(),
                        channel::Event::Msg(timer) => {
                            let mut runnable = Some(timer.runnable);
                            timer_handle
                                .insert_source(
                                    calloop::timer::Timer::from_duration(timer.duration),
                                    move |_, _, _| {
                                        if let Some(runnable) = runnable.take() {
                                            run(runnable);
                                        }
                                        TimeoutAction::Drop
                                    },
                                )
                                .expect("Failed to start timer");
                        }
                    })
                    .expect("Failed to start timer thread");
                if let Err(error) = event_loop.run(None, &mut (), |_| {}) {
                    log::error!("gpui_android: timer loop: {error}");
                }
            })
            .expect("spawn the GPUI timer thread");
        background_threads.push(timer_thread);

        Self {
            main_sender,
            main_wake,
            timer_sender,
            background_sender,
            _background_threads: background_threads,
            main_thread_id: thread::current().id(),
        }
    }
}

impl PlatformDispatcher for AndroidDispatcher {
    fn is_main_thread(&self) -> bool {
        thread::current().id() == self.main_thread_id
    }

    fn dispatch(&self, runnable: RunnableVariant, priority: Priority) {
        self.background_sender
            .send(priority, runnable)
            .unwrap_or_else(|_| panic!("blocking sender returned without value"));
    }

    fn dispatch_on_main_thread(&self, runnable: RunnableVariant, priority: Priority) {
        match self.main_sender.send(priority, runnable) {
            Ok(()) => bridge::signal(self.main_wake.as_raw_fd()),
            // The main loop is gone, so this is a background thread at shutdown; the runnable
            // may hold a !Send future, which must not drop here.
            Err(runnable) => std::mem::forget(runnable),
        }
    }

    fn dispatch_after(&self, duration: Duration, runnable: RunnableVariant) {
        if let Err(error) = self.timer_sender.send(TimerAfter { duration, runnable }) {
            // Leaked rather than dropped: dropping would cancel the task and panic its awaiter.
            std::mem::forget(error);
        }
    }

    fn spawn_realtime(&self, f: Box<dyn FnOnce() + Send>) {
        thread::spawn(move || {
            // SAFETY: pthread_self is always safe to call.
            let thread_id = unsafe { libc::pthread_self() };
            // SAFETY: all sched_param members are valid when zeroed.
            let mut param = unsafe { MaybeUninit::<libc::sched_param>::zeroed().assume_init() };
            param.sched_priority = 65;
            // SAFETY: param is initialised and thread_id is this thread.
            if unsafe { libc::pthread_setschedparam(thread_id, libc::SCHED_FIFO, &param) } != 0 {
                log::warn!("gpui_android: no realtime priority for this thread");
            }
            f();
        });
    }
}
