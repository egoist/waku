//! In-app updates via Sparkle.
//!
//! `scripts/bundle.sh` embeds Sparkle.framework at Contents/Frameworks, and
//! this module loads it at runtime instead of linking it, so a bare `cargo
//! run` binary simply runs without an updater. Sparkle still owns update
//! discovery, download, signature verification, installation, and relaunch.
//! Waku's routing user driver keeps automatic checks in the sidebar, but
//! forwards an explicit Check for Updates action to Sparkle's standard user
//! driver so the original updater window still appears when requested.
//!
//! Debug builds stay dormant so the dev watcher's app never offers to replace
//! itself with a production build. `WAKU_PREVIEW_UPDATE=1` fakes only the
//! automatic sidebar result while retaining the real Sparkle flow for the
//! Check for Updates menu; `WAKU_FORCE_UPDATER=1` exercises everything for
//! real from a debug bundle.

use gpui::Global;

/// App-wide handle to the updater, if this build can update itself.
pub struct UpdaterState(pub Option<Updater>);

impl Global for UpdaterState {}

/// The compact state rendered by Waku. Update details remain owned by
/// Sparkle and never enter a frame path.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum UpdateStatus {
    #[default]
    Idle,
    Checking,
    Available,
    Updating,
}

#[derive(Clone, Debug)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub enum UpdaterEvent {
    StatusChanged(UpdateStatus),
    UpToDate,
    Failed(String),
}

#[cfg(target_os = "macos")]
mod macos {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use block2::{DynBlock, RcBlock};
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, NSObject, NSObjectProtocol};
    use objc2::{
        DefinedClass, MainThreadMarker, MainThreadOnly, define_class, extern_protocol, msg_send,
        sel,
    };
    use objc2_foundation::NSString;

    use super::{UpdateStatus, UpdaterEvent};

    const USER_UPDATE_CHOICE_INSTALL: isize = 1;
    const UPDATE_CHECK_USER_INITIATED: isize = 0;
    const UPDATE_CHECK_IN_BACKGROUND: isize = 1;
    const MANUAL_CHECK_MAX_RETRIES: u16 = 200;

    extern_protocol!(
        /// Dynamically loaded from the embedded Sparkle framework.
        unsafe trait SPUUserDriver: NSObjectProtocol {}
    );

    extern_protocol!(
        /// Only the update-cycle completion callback is implemented below.
        unsafe trait SPUUpdaterDelegate: NSObjectProtocol {}
    );

    struct PendingUpdate {
        appcast_item: Retained<AnyObject>,
        state: Retained<AnyObject>,
        reply: RcBlock<dyn Fn(isize)>,
    }

    struct UserDriverIvars {
        /// Explicit checks and the one-time automatic-check permission prompt
        /// use Sparkle's own windows. Scheduled checks stay inside Waku.
        standard_driver: Retained<AnyObject>,
        standard_presentation: Cell<bool>,
        standard_update_check: Cell<Option<isize>>,
        manual_check_requested: Rc<Cell<bool>>,
        manual_check_retry_count: Cell<u16>,
        pending_update: RefCell<Option<PendingUpdate>>,
        status: Rc<Cell<UpdateStatus>>,
        events: smol::channel::Sender<UpdaterEvent>,
    }

    define_class!(
        #[unsafe(super(NSObject))]
        #[name = "WakuSparkleUserDriver"]
        #[thread_kind = MainThreadOnly]
        #[ivars = UserDriverIvars]
        struct UserDriver;

        unsafe impl SPUUserDriver for UserDriver {
            #[unsafe(method(showUpdatePermissionRequest:reply:))]
            fn show_update_permission_request(
                &self,
                request: &AnyObject,
                reply: &DynBlock<dyn Fn(*mut AnyObject)>,
            ) {
                let _: () = unsafe {
                    msg_send![
                        &*self.ivars().standard_driver,
                        showUpdatePermissionRequest: request,
                        reply: reply
                    ]
                };
            }

            #[unsafe(method(showUserInitiatedUpdateCheckWithCancellation:))]
            fn show_user_initiated_update_check(&self, cancellation: &DynBlock<dyn Fn()>) {
                self.begin_standard_presentation(UPDATE_CHECK_USER_INITIATED);
                let _: () = unsafe {
                    msg_send![
                        &*self.ivars().standard_driver,
                        showUserInitiatedUpdateCheckWithCancellation: cancellation
                    ]
                };
            }

            #[unsafe(method(showUpdateFoundWithAppcastItem:state:reply:))]
            fn show_update_found(
                &self,
                appcast_item: &AnyObject,
                state: &AnyObject,
                reply: &DynBlock<dyn Fn(isize)>,
            ) {
                if self.ivars().manual_check_requested.replace(false) {
                    self.begin_standard_presentation(UPDATE_CHECK_IN_BACKGROUND);
                    self.show_update_found_with_standard_driver(appcast_item, state, reply);
                    return;
                }
                if self.uses_standard_presentation() {
                    self.show_update_found_with_standard_driver(appcast_item, state, reply);
                    return;
                }

                let appcast_item = unsafe {
                    Retained::retain(std::ptr::from_ref(appcast_item).cast_mut())
                        .expect("Sparkle supplied a non-null appcast item")
                };
                let state = unsafe {
                    Retained::retain(std::ptr::from_ref(state).cast_mut())
                        .expect("Sparkle supplied a non-null update state")
                };
                self.ivars().pending_update.replace(Some(PendingUpdate {
                    appcast_item,
                    state,
                    reply: reply.copy(),
                }));
                self.set_status(UpdateStatus::Available);
            }

            #[unsafe(method(showUpdateReleaseNotesWithDownloadData:))]
            fn show_update_release_notes(&self, download_data: &AnyObject) {
                if self.uses_standard_presentation() {
                    let _: () = unsafe {
                        msg_send![
                            &*self.ivars().standard_driver,
                            showUpdateReleaseNotesWithDownloadData: download_data
                        ]
                    };
                }
            }

            #[unsafe(method(showUpdateReleaseNotesFailedToDownloadWithError:))]
            fn show_update_release_notes_failed(&self, error: &AnyObject) {
                if self.uses_standard_presentation() {
                    let _: () = unsafe {
                        msg_send![
                            &*self.ivars().standard_driver,
                            showUpdateReleaseNotesFailedToDownloadWithError: error
                        ]
                    };
                }
            }

            #[unsafe(method(showUpdateNotFoundWithError:acknowledgement:))]
            fn show_update_not_found(
                &self,
                error: &AnyObject,
                acknowledgement: &DynBlock<dyn Fn()>,
            ) {
                if self.ivars().manual_check_requested.replace(false) {
                    self.begin_standard_presentation(UPDATE_CHECK_IN_BACKGROUND);
                }
                if self.uses_standard_presentation() {
                    let _: () = unsafe {
                        msg_send![
                            &*self.ivars().standard_driver,
                            showUpdateNotFoundWithError: error,
                            acknowledgement: acknowledgement
                        ]
                    };
                    return;
                }

                self.clear_update();
                self.send(UpdaterEvent::UpToDate);
                acknowledgement.call(());
            }

            #[unsafe(method(showUpdaterError:acknowledgement:))]
            fn show_updater_error(
                &self,
                error: &AnyObject,
                acknowledgement: &DynBlock<dyn Fn()>,
            ) {
                if self.ivars().manual_check_requested.replace(false) {
                    self.begin_standard_presentation(UPDATE_CHECK_IN_BACKGROUND);
                }
                if self.uses_standard_presentation() {
                    let _: () = unsafe {
                        msg_send![
                            &*self.ivars().standard_driver,
                            showUpdaterError: error,
                            acknowledgement: acknowledgement
                        ]
                    };
                    return;
                }

                self.clear_update();
                self.send(UpdaterEvent::Failed(error_description(error)));
                acknowledgement.call(());
            }

            #[unsafe(method(showDownloadInitiatedWithCancellation:))]
            fn show_download_initiated(&self, cancellation: &DynBlock<dyn Fn()>) {
                if self.uses_standard_presentation() {
                    let _: () = unsafe {
                        msg_send![
                            &*self.ivars().standard_driver,
                            showDownloadInitiatedWithCancellation: cancellation
                        ]
                    };
                    return;
                }
                self.set_status(UpdateStatus::Updating);
            }

            #[unsafe(method(showDownloadDidReceiveExpectedContentLength:))]
            fn show_expected_content_length(&self, expected_content_length: u64) {
                if self.uses_standard_presentation() {
                    let _: () = unsafe {
                        msg_send![
                            &*self.ivars().standard_driver,
                            showDownloadDidReceiveExpectedContentLength: expected_content_length
                        ]
                    };
                }
            }

            #[unsafe(method(showDownloadDidReceiveDataOfLength:))]
            fn show_downloaded_data(&self, length: u64) {
                if self.uses_standard_presentation() {
                    let _: () = unsafe {
                        msg_send![
                            &*self.ivars().standard_driver,
                            showDownloadDidReceiveDataOfLength: length
                        ]
                    };
                }
            }

            #[unsafe(method(showDownloadDidStartExtractingUpdate))]
            fn show_extracting_update(&self) {
                if self.uses_standard_presentation() {
                    let _: () = unsafe {
                        msg_send![&*self.ivars().standard_driver, showDownloadDidStartExtractingUpdate]
                    };
                    return;
                }
                self.set_status(UpdateStatus::Updating);
            }

            #[unsafe(method(showExtractionReceivedProgress:))]
            fn show_extraction_progress(&self, progress: f64) {
                if self.uses_standard_presentation() {
                    let _: () = unsafe {
                        msg_send![
                            &*self.ivars().standard_driver,
                            showExtractionReceivedProgress: progress
                        ]
                    };
                }
            }

            #[unsafe(method(showReadyToInstallAndRelaunch:))]
            fn show_ready_to_install(&self, reply: &DynBlock<dyn Fn(isize)>) {
                if self.uses_standard_presentation() {
                    let _: () = unsafe {
                        msg_send![
                            &*self.ivars().standard_driver,
                            showReadyToInstallAndRelaunch: reply
                        ]
                    };
                    return;
                }
                self.set_status(UpdateStatus::Updating);
                reply.call((USER_UPDATE_CHOICE_INSTALL,));
            }

            #[unsafe(method(showInstallingUpdateWithApplicationTerminated:retryTerminatingApplication:))]
            fn show_installing_update(
                &self,
                application_terminated: bool,
                retry_terminating_application: &DynBlock<dyn Fn()>,
            ) {
                if self.uses_standard_presentation() {
                    let _: () = unsafe {
                        msg_send![
                            &*self.ivars().standard_driver,
                            showInstallingUpdateWithApplicationTerminated: application_terminated,
                            retryTerminatingApplication: retry_terminating_application
                        ]
                    };
                    return;
                }
                self.set_status(UpdateStatus::Updating);
            }

            #[unsafe(method(showUpdateInstalledAndRelaunched:acknowledgement:))]
            fn show_update_installed(
                &self,
                relaunched: bool,
                acknowledgement: &DynBlock<dyn Fn()>,
            ) {
                if self.uses_standard_presentation() {
                    let _: () = unsafe {
                        msg_send![
                            &*self.ivars().standard_driver,
                            showUpdateInstalledAndRelaunched: relaunched,
                            acknowledgement: acknowledgement
                        ]
                    };
                    return;
                }
                acknowledgement.call(());
            }

            #[unsafe(method(dismissUpdateInstallation))]
            fn dismiss_update_installation(&self) {
                if self.uses_standard_presentation() {
                    let _: () = unsafe {
                        msg_send![&*self.ivars().standard_driver, dismissUpdateInstallation]
                    };
                    self.ivars().standard_presentation.set(false);
                    self.ivars().standard_update_check.set(None);
                }
                self.clear_update();
            }

            #[unsafe(method(showUpdateInFocus))]
            fn show_update_in_focus(&self) {
                if !self.uses_standard_presentation() && self.present_pending_update() {
                    return;
                }
                let _: () = unsafe {
                    msg_send![&*self.ivars().standard_driver, showUpdateInFocus]
                };
            }
        }

        impl UserDriver {
            #[unsafe(method(startRequestedStandardUpdateCheck:))]
            fn start_requested_standard_update_check(&self, updater: &AnyObject) {
                if !self.ivars().manual_check_requested.get() {
                    return;
                }
                if self.present_pending_update() {
                    self.ivars().manual_check_requested.set(false);
                    return;
                }

                let can_check: bool = unsafe { msg_send![updater, canCheckForUpdates] };
                if can_check {
                    self.ivars().manual_check_requested.set(false);
                    self.begin_standard_presentation(UPDATE_CHECK_USER_INITIATED);
                    let _: () = unsafe { msg_send![updater, checkForUpdates] };
                } else if self.ivars().manual_check_retry_count.get()
                    < MANUAL_CHECK_MAX_RETRIES
                {
                    self.ivars()
                        .manual_check_retry_count
                        .set(self.ivars().manual_check_retry_count.get() + 1);
                    self.schedule_requested_standard_check(updater, 0.05);
                } else {
                    self.ivars().manual_check_requested.set(false);
                    let _: () = unsafe {
                        msg_send![&*self.ivars().standard_driver, dismissUpdateInstallation]
                    };
                }
            }
        }

        unsafe impl SPUUpdaterDelegate for UserDriver {
            #[unsafe(method(updater:didFinishUpdateCycleForUpdateCheck:error:))]
            fn did_finish_update_cycle(
                &self,
                _updater: &AnyObject,
                update_check: isize,
                _error: Option<&AnyObject>,
            ) {
                if self.ivars().standard_update_check.get() == Some(update_check) {
                    self.ivars().standard_presentation.set(false);
                    self.ivars().standard_update_check.set(None);
                }
            }
        }

        unsafe impl NSObjectProtocol for UserDriver {}
    );

    impl UserDriver {
        fn new(
            mtm: MainThreadMarker,
            standard_driver: Retained<AnyObject>,
            status: Rc<Cell<UpdateStatus>>,
            events: smol::channel::Sender<UpdaterEvent>,
        ) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(UserDriverIvars {
                standard_driver,
                standard_presentation: Cell::new(false),
                standard_update_check: Cell::new(None),
                manual_check_requested: Rc::new(Cell::new(false)),
                manual_check_retry_count: Cell::new(0),
                pending_update: RefCell::new(None),
                status,
                events,
            });
            unsafe { msg_send![super(this), init] }
        }

        fn uses_standard_presentation(&self) -> bool {
            self.ivars().standard_presentation.get()
        }

        fn begin_standard_presentation(&self, update_check: isize) {
            self.ivars().standard_presentation.set(true);
            self.ivars().standard_update_check.set(Some(update_check));
        }

        fn schedule_requested_standard_check(&self, updater: &AnyObject, delay: f64) {
            let _: () = unsafe {
                msg_send![
                    self,
                    performSelector: sel!(startRequestedStandardUpdateCheck:),
                    withObject: updater,
                    afterDelay: delay
                ]
            };
        }

        fn show_update_found_with_standard_driver(
            &self,
            appcast_item: &AnyObject,
            state: &AnyObject,
            reply: &DynBlock<dyn Fn(isize)>,
        ) {
            let _: () = unsafe {
                msg_send![
                    &*self.ivars().standard_driver,
                    showUpdateFoundWithAppcastItem: appcast_item,
                    state: state,
                    reply: reply
                ]
            };
            // A result discovered by the silent checker still has
            // `state.userInitiated == false`, so the standard driver may apply
            // its gentle-reminder rules and leave the alert hidden. The menu
            // action is explicit, so bring the newly created alert forward.
            let _: () = unsafe { msg_send![&*self.ivars().standard_driver, showUpdateInFocus] };
        }

        /// Promote the result already held by a silent automatic check into
        /// Sparkle's standard updater window without discarding it and
        /// starting a second network request.
        fn present_pending_update(&self) -> bool {
            let Some(update) = self.ivars().pending_update.borrow_mut().take() else {
                return false;
            };

            self.begin_standard_presentation(UPDATE_CHECK_IN_BACKGROUND);
            self.set_status(UpdateStatus::Idle);
            self.show_update_found_with_standard_driver(
                &update.appcast_item,
                &update.state,
                &update.reply,
            );
            true
        }

        /// Show Sparkle's standard checking UI immediately, then start a real
        /// user-initiated check as soon as any silent automatic session has
        /// finished tearing down.
        fn request_standard_check(&self, updater: &AnyObject) -> bool {
            if self.present_pending_update() {
                return true;
            }
            if self.uses_standard_presentation() {
                let _: () = unsafe { msg_send![updater, checkForUpdates] };
                return true;
            }
            if self.ivars().manual_check_requested.get() {
                let _: () = unsafe { msg_send![&*self.ivars().standard_driver, showUpdateInFocus] };
                return true;
            }
            if self.ivars().status.get() == UpdateStatus::Updating {
                return false;
            }

            let can_check: bool = unsafe { msg_send![updater, canCheckForUpdates] };
            if can_check {
                self.begin_standard_presentation(UPDATE_CHECK_USER_INITIATED);
                let _: () = unsafe { msg_send![updater, checkForUpdates] };
                return true;
            }

            self.ivars().manual_check_requested.set(true);
            self.ivars().manual_check_retry_count.set(0);
            let manual_check_requested = self.ivars().manual_check_requested.clone();
            let cancellation: RcBlock<dyn Fn()> = RcBlock::new(move || {
                manual_check_requested.set(false);
            });
            let _: () = unsafe {
                msg_send![
                    &*self.ivars().standard_driver,
                    showUserInitiatedUpdateCheckWithCancellation: &*cancellation
                ]
            };

            self.schedule_requested_standard_check(updater, 0.0);
            true
        }

        fn send(&self, event: UpdaterEvent) {
            let _ = self.ivars().events.try_send(event);
        }

        fn set_status(&self, status: UpdateStatus) {
            if self.ivars().status.replace(status) != status {
                self.send(UpdaterEvent::StatusChanged(status));
            }
        }

        fn clear_update(&self) {
            self.ivars().pending_update.borrow_mut().take();
            self.set_status(UpdateStatus::Idle);
        }

        fn install_available_update(&self) -> bool {
            if self.uses_standard_presentation() {
                return false;
            }
            let Some(update) = self.ivars().pending_update.borrow_mut().take() else {
                return false;
            };
            self.set_status(UpdateStatus::Updating);
            update.reply.call((USER_UPDATE_CHOICE_INSTALL,));
            true
        }
    }

    pub struct Updater {
        updater: Option<Retained<AnyObject>>,
        user_driver: Option<Retained<UserDriver>>,
        status: Rc<Cell<UpdateStatus>>,
        events: smol::channel::Receiver<UpdaterEvent>,
        preview_events: Option<smol::channel::Sender<UpdaterEvent>>,
    }

    impl Updater {
        /// Load Sparkle and start its updater. Returns `None` when this build
        /// cannot update itself: debug builds unless forced, and binaries
        /// running outside a bundle with an embedded framework.
        pub fn init() -> Option<Self> {
            let preview = cfg!(debug_assertions)
                && std::env::var_os("WAKU_PREVIEW_UPDATE").is_some_and(|value| value == "1");
            let forced = std::env::var_os("WAKU_FORCE_UPDATER").is_some_and(|value| value == "1");
            if cfg!(debug_assertions) && !forced && !preview {
                return None;
            }

            let mtm = MainThreadMarker::new()?;
            let library = sparkle_library_path()?;
            let library_c =
                std::ffi::CString::new(std::os::unix::ffi::OsStrExt::as_bytes(library.as_os_str()))
                    .ok()?;
            let handle = unsafe { libc::dlopen(library_c.as_ptr(), libc::RTLD_NOW) };
            if handle.is_null() {
                let reason = unsafe { libc::dlerror() };
                let reason = if reason.is_null() {
                    "unknown dlopen failure".into()
                } else {
                    unsafe { std::ffi::CStr::from_ptr(reason) }
                        .to_string_lossy()
                        .into_owned()
                };
                eprintln!("Waku updater: failed to load Sparkle: {reason}");
                return None;
            }

            let bundle_class = AnyClass::get(c"NSBundle")?;
            let updater_class = AnyClass::get(c"SPUUpdater")?;
            let standard_driver_class = AnyClass::get(c"SPUStandardUserDriver")?;
            let main_bundle: *mut AnyObject = unsafe { msg_send![bundle_class, mainBundle] };
            if main_bundle.is_null() {
                return None;
            }

            let standard_driver = unsafe {
                let allocated: *mut AnyObject = msg_send![standard_driver_class, alloc];
                let initialized: *mut AnyObject = msg_send![
                    allocated,
                    initWithHostBundle: main_bundle,
                    delegate: std::ptr::null_mut::<AnyObject>()
                ];
                Retained::from_raw(initialized)?
            };

            let status = Rc::new(Cell::new(if preview {
                UpdateStatus::Available
            } else {
                UpdateStatus::Idle
            }));
            let (event_tx, events) = smol::channel::unbounded();
            let preview_events = preview.then(|| event_tx.clone());
            let user_driver = UserDriver::new(mtm, standard_driver, status.clone(), event_tx);
            let updater = unsafe {
                let allocated: *mut AnyObject = msg_send![updater_class, alloc];
                let initialized: *mut AnyObject = msg_send![
                    allocated,
                    initWithHostBundle: main_bundle,
                    applicationBundle: main_bundle,
                    userDriver: &*user_driver,
                    delegate: &*user_driver
                ];
                Retained::from_raw(initialized)?
            };

            let started: bool = unsafe {
                msg_send![
                    &*updater,
                    startUpdater: std::ptr::null_mut::<*mut AnyObject>()
                ]
            };
            if !started {
                eprintln!("Waku updater: Sparkle rejected its updater configuration");
                return None;
            }

            let updater = Self {
                updater: Some(updater),
                user_driver: Some(user_driver),
                status,
                events,
                preview_events,
            };

            // Starting only arms the scheduled checker, which stays quiet
            // until its interval has elapsed since the last check. Force one
            // silent check per launch once the user has consented.
            if !preview && updater.automatically_checks_for_updates() {
                let sparkle = updater
                    .updater
                    .as_ref()
                    .expect("Sparkle updater initialized");
                let _: () = unsafe { msg_send![&**sparkle, checkForUpdatesInBackground] };
            }

            Some(updater)
        }

        /// Run a user-initiated check through Sparkle's standard updater UI.
        pub fn check_for_updates(&self) {
            if let Some(updater) = &self.updater {
                // Preview mode fakes only the automatic result. Hide that
                // placeholder before handing the explicit action to Sparkle.
                if self.preview_events.is_some() {
                    self.set_preview_status(UpdateStatus::Idle);
                }
                if let Some(user_driver) = &self.user_driver {
                    user_driver.request_standard_check(updater);
                }
            } else {
                self.set_preview_status(UpdateStatus::Available);
            }
        }

        pub fn install_available_update(&self) -> bool {
            if self
                .user_driver
                .as_ref()
                .is_some_and(|user_driver| user_driver.install_available_update())
            {
                return true;
            }
            if self.preview_events.is_some() && self.status.get() == UpdateStatus::Available {
                self.set_preview_status(UpdateStatus::Updating);
                true
            } else {
                false
            }
        }

        pub fn status(&self) -> UpdateStatus {
            self.status.get()
        }

        pub fn events(&self) -> smol::channel::Receiver<UpdaterEvent> {
            self.events.clone()
        }

        /// Whether Sparkle checks for updates on its own schedule. Sparkle
        /// owns the persisted value in this app's user defaults.
        pub fn automatically_checks_for_updates(&self) -> bool {
            self.updater.as_ref().is_some_and(|updater| unsafe {
                msg_send![&**updater, automaticallyChecksForUpdates]
            })
        }

        pub fn set_automatically_checks_for_updates(&self, enabled: bool) {
            if let Some(updater) = &self.updater {
                let _: () =
                    unsafe { msg_send![&**updater, setAutomaticallyChecksForUpdates: enabled] };
            }
        }

        #[cfg(test)]
        fn preview() -> Self {
            let status = Rc::new(Cell::new(UpdateStatus::Available));
            let (preview_events, events) = smol::channel::unbounded();
            Self {
                updater: None,
                user_driver: None,
                status,
                events,
                preview_events: Some(preview_events),
            }
        }

        fn set_preview_status(&self, status: UpdateStatus) {
            if self.status.replace(status) != status
                && let Some(events) = &self.preview_events
            {
                let _ = events.try_send(UpdaterEvent::StatusChanged(status));
            }
        }
    }

    fn error_description(error: &AnyObject) -> String {
        let description: *mut NSString = unsafe { msg_send![error, localizedDescription] };
        unsafe { description.as_ref() }
            .map(ToString::to_string)
            .unwrap_or_else(|| "Unknown updater error".to_owned())
    }

    /// The embedded framework's dylib next to the running executable
    /// (Contents/MacOS/Waku → Contents/Frameworks/Sparkle.framework/Sparkle).
    fn sparkle_library_path() -> Option<std::path::PathBuf> {
        let executable = std::env::current_exe().ok()?;
        let contents = executable.parent()?.parent()?;
        let library = contents.join("Frameworks/Sparkle.framework/Sparkle");
        library.exists().then_some(library)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use objc2::ClassType;

        #[test]
        fn routing_user_driver_satisfies_sparkle_protocols() {
            let target_dir = std::env::var_os("CARGO_TARGET_DIR")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target"));
            let library = target_dir
                .join("debug/Waku Debug.app/Contents/Frameworks/Sparkle.framework/Sparkle");
            if !library.exists() {
                return;
            }

            let library_c =
                std::ffi::CString::new(std::os::unix::ffi::OsStrExt::as_bytes(library.as_os_str()))
                    .expect("Sparkle path must not contain a null byte");
            let handle = unsafe { libc::dlopen(library_c.as_ptr(), libc::RTLD_NOW) };
            assert!(!handle.is_null(), "embedded Sparkle framework must load");

            // Registration validates every required SPUUserDriver selector
            // against the live protocol and panics if one is misplaced.
            let _ = UserDriver::class();
        }

        #[test]
        fn preview_update_switches_from_available_to_spinner() {
            let updater = Updater::preview();
            assert_eq!(updater.status(), UpdateStatus::Available);
            assert!(updater.install_available_update());
            assert_eq!(updater.status(), UpdateStatus::Updating);
            assert!(matches!(
                updater.events().try_recv(),
                Ok(UpdaterEvent::StatusChanged(UpdateStatus::Updating))
            ));
        }
    }
}

#[cfg(target_os = "macos")]
pub use macos::Updater;

/// Non-macOS builds have no updater yet. This stub is the seam where a
/// platform implementation slots in (WinSparkle consumes the same appcast
/// format on Windows); callers already treat `None` as "no updater".
#[cfg(not(target_os = "macos"))]
pub struct Updater;

#[cfg(not(target_os = "macos"))]
impl Updater {
    pub fn init() -> Option<Self> {
        None
    }

    pub fn check_for_updates(&self) {}

    pub fn install_available_update(&self) -> bool {
        false
    }

    pub fn status(&self) -> UpdateStatus {
        UpdateStatus::Idle
    }

    pub fn events(&self) -> smol::channel::Receiver<UpdaterEvent> {
        let (_tx, rx) = smol::channel::unbounded();
        rx
    }

    pub fn automatically_checks_for_updates(&self) -> bool {
        false
    }

    pub fn set_automatically_checks_for_updates(&self, _enabled: bool) {}
}
