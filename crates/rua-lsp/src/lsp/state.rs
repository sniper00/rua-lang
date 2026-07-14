use super::*;

#[derive(Clone)]
pub(super) struct DiskFile {
    pub(super) path: PathBuf,
    pub(super) analysis_path: PathBuf,
    pub(super) text: String,
    pub(super) source_root: SourceRootId,
    pub(super) kind: FileKind,
}

#[derive(Clone)]
pub(super) struct WorkspaceProject {
    pub(super) root_file: FileId,
    pub(super) workspace_roots: Vec<ProjectRoot>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WatchOperation {
    Register,
    Unregister,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct WatchRegistrationState {
    pub(super) desired: bool,
    pub(super) register_result: Option<bool>,
    pub(super) unregister_result: Option<bool>,
}

impl WatchRegistrationState {
    #[cfg(test)]
    pub(super) fn is_active(&self) -> bool {
        self.register_result == Some(true) && self.unregister_result != Some(true)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct WatchRegistrationFailure {
    pub(super) operation: WatchOperation,
    pub(super) registration_id: String,
    pub(super) code: i32,
    pub(super) message: String,
}

#[derive(Clone, Debug)]
pub(super) struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    pub(super) fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(super) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub(super) fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

pub(super) struct PendingQuery {
    pub(super) request_id: RequestId,
    pub(super) input_generation: u64,
    pub(super) cancellation: CancellationToken,
}

pub(super) enum BackgroundResult {
    References {
        task_id: u64,
        result: Option<Vec<ReferenceResult>>,
    },
    WorkspaceSymbols {
        task_id: u64,
        result: Option<Vec<AnalysisWorkspaceSymbol>>,
    },
    WorkspaceScan {
        generation: u64,
        result: Option<Vec<crate::filesystem::WorkspaceScan>>,
    },
    LibraryScan {
        generation: u64,
        result: Result<crate::filesystem::LibraryConfig, String>,
    },
}

pub(super) struct Server {
    pub(super) connection: Connection,
    pub(super) host: AnalysisHost,
    pub(super) file_ids: HashMap<PathBuf, (Uri, FileId)>,
    pub(super) open_buffers: HashMap<FileId, (Uri, String)>,
    pub(super) open_versions: HashMap<FileId, i32>,
    pub(super) disk_files: HashMap<FileId, DiskFile>,
    pub(super) file_to_uri: HashMap<FileId, Uri>,
    pub(super) next_file_id: u32,
    pub(super) next_root_id: u32,
    pub(super) projects: BTreeMap<ProjectId, WorkspaceProject>,
    pub(super) file_projects: HashMap<FileId, ProjectId>,
    pub(super) project_dependency_roots: BTreeMap<ProjectId, Vec<ProjectRoot>>,
    pub(super) library_roots: Vec<PathBuf>,
    pub(super) library_mounts: HashMap<String, PathBuf>,
    pub(super) library_bases: Vec<PathBuf>,
    pub(super) library_project_bases: BTreeMap<u32, Vec<PathBuf>>,
    pub(super) library_source_root: Option<SourceRootId>,
    pub(super) library_file_ids: HashSet<FileId>,
    pub(super) watched_paths: Vec<PathBuf>,
    pub(super) watch_registration_id: Option<String>,
    pub(super) watch_registrations: HashMap<String, WatchRegistrationState>,
    pub(super) pending_watch_requests: HashMap<RequestId, (WatchOperation, String)>,
    pub(super) last_watch_failure: Option<WatchRegistrationFailure>,
    pub(super) next_request_id: i32,
    pub(super) input_generation: u64,
    pub(super) next_task_id: u64,
    pub(super) pending_queries: HashMap<u64, PendingQuery>,
    pub(super) next_scan_generation: u64,
    pub(super) workspace_scan: Option<(u64, CancellationToken)>,
    pub(super) library_scan: Option<(u64, CancellationToken)>,
    pub(super) worker_pool: WorkerPool,
    pub(super) background_sender: crossbeam_channel::Sender<BackgroundResult>,
    pub(super) background_receiver: crossbeam_channel::Receiver<BackgroundResult>,
    pub(super) line_indices: RefCell<LineIndexCache>,
}

impl Server {
    pub(super) fn new(connection: Connection) -> Self {
        let (background_sender, background_receiver) = crossbeam_channel::unbounded();
        let worker_count = std::thread::available_parallelism()
            .map(|count| count.get().clamp(2, 4))
            .unwrap_or(2);
        Self {
            connection,
            host: AnalysisHost::new(),
            file_ids: HashMap::new(),
            open_buffers: HashMap::new(),
            open_versions: HashMap::new(),
            disk_files: HashMap::new(),
            file_to_uri: HashMap::new(),
            next_file_id: 0,
            next_root_id: 1,
            projects: BTreeMap::new(),
            file_projects: HashMap::new(),
            project_dependency_roots: BTreeMap::new(),
            library_roots: Vec::new(),
            library_mounts: HashMap::new(),
            library_bases: Vec::new(),
            library_project_bases: BTreeMap::new(),
            library_source_root: None,
            library_file_ids: HashSet::new(),
            watched_paths: Vec::new(),
            watch_registration_id: None,
            watch_registrations: HashMap::new(),
            pending_watch_requests: HashMap::new(),
            last_watch_failure: None,
            next_request_id: 1,
            input_generation: 0,
            next_task_id: 1,
            pending_queries: HashMap::new(),
            next_scan_generation: 1,
            workspace_scan: None,
            library_scan: None,
            worker_pool: WorkerPool::new(worker_count, worker_count * 4),
            background_sender,
            background_receiver,
            line_indices: RefCell::new(LineIndexCache::default()),
        }
    }
}
