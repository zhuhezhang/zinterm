/// One process row sampled from the remote `ps` (#23). CPU/mem are percentages
/// as reported by `ps` (pcpu/pmem); `command` is the (width-truncated) args.
#[derive(Debug, Clone)]
pub struct ProcInfo {
    pub pid: u32,
    pub user: String,
    pub cpu: f32,
    pub mem: f32,
    pub command: String,
}

#[derive(Debug, Clone, Default)]
pub struct SystemDetails {
    pub overview: Vec<(String, String)>,
    pub cpu_info: Vec<(String, String)>,
    pub gpu_info: Vec<(String, String)>,
    pub cpu_usage: Vec<(String, String)>,
    pub memory: Vec<(String, String)>,
    pub swap: Vec<(String, String)>,
    pub networks: Vec<(String, String, String, String, String)>,
    pub filesystems: Vec<(String, String, String, String, String)>,
}
