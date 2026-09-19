use std::path::PathBuf;

use tokio::sync::mpsc::UnboundedSender;

use super::super::transfer::{DownloadConflict, SftpCommand, SftpHandle};

impl SftpHandle {
    pub fn list_dir(&self, path: String) {
        let _ = self.commands.send(SftpCommand::ListDir(path));
    }
    pub fn refresh_dir(&self, path: String) {
        let _ = self.commands.send(SftpCommand::RefreshDir(path));
    }
    pub fn download(&self, remote: String, local_dir: String, conflict: DownloadConflict) {
        let _ = self.commands.send(SftpCommand::Download {
            remote,
            local_dir,
            conflict,
        });
    }
    pub fn download_archive(&self, remote_dir: String, names: Vec<String>, local_dir: String) {
        let _ = self.commands.send(SftpCommand::DownloadArchive {
            remote_dir,
            names,
            local_dir,
        });
    }
    pub fn cancel_transfer(&self, id: String) {
        let _ = self.commands.send(SftpCommand::CancelTransfer(id));
    }
    pub fn upload(&self, local: PathBuf, remote_dir: String) {
        let _ = self.commands.send(SftpCommand::Upload {
            local,
            remote_dir,
            cleanup_after: None,
        });
    }
    pub fn copy_to(
        &self,
        remotes: Vec<String>,
        target: UnboundedSender<SftpCommand>,
        target_dir: String,
    ) {
        let _ = self.commands.send(SftpCommand::CopyTo {
            remotes,
            target,
            target_dir,
        });
    }
    pub fn toggle_tree_node(&self, path: String) {
        let _ = self.commands.send(SftpCommand::ToggleTreeNode(path));
    }
    pub fn delete(&self, path: String) {
        let _ = self.commands.send(SftpCommand::Delete(path));
    }
    pub fn open_temp(&self, remote: String, edit: bool) {
        let _ = self.commands.send(SftpCommand::OpenTemp { remote, edit });
    }
    pub fn rename(&self, from: String, to: String) {
        let _ = self.commands.send(SftpCommand::Rename { from, to });
    }
    pub fn chmod(&self, path: String, mode: u32) {
        let _ = self.commands.send(SftpCommand::Chmod { path, mode });
    }
    pub fn mkdir(&self, path: String) {
        let _ = self.commands.send(SftpCommand::MkDir(path));
    }
    pub fn touch(&self, path: String) {
        let _ = self.commands.send(SftpCommand::TouchFile(path));
    }
    pub fn read_text(&self, remote: String, edit: bool) {
        let _ = self.commands.send(SftpCommand::ReadText { remote, edit });
    }
    pub fn write_text(&self, remote: String, content: String) {
        let _ = self
            .commands
            .send(SftpCommand::WriteText { remote, content });
    }
    pub fn close(&self) {
        let _ = self.commands.send(SftpCommand::Close);
    }
}
