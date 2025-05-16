use bitcoin::network::Network;
use lightning::ln::msgs::SocketAddress;
use lightning::util::logger::Level;
use std::fs;
use std::path::PathBuf;
use tempfile::{Builder, TempDir};

pub struct LdkUserInfo {
	pub bitcoind_rpc_username: String,
	pub bitcoind_rpc_password: String,
	pub bitcoind_rpc_port: u16,
	pub bitcoind_rpc_host: String,
	pub ldk_data_dir: PathBuf,
	pub ldk_peer_listening_port: u16,
	pub ldk_announced_listen_addr: Vec<SocketAddress>,
	pub ldk_announced_node_name: [u8; 32],
	pub network: Network,
	pub log_level: Level,
	pub node_num: u8,
}

// Here we initialize three layers of directories needed for our tests. We won't persist ldk data, but we'll persist
// the logs to help with debugging.
pub(crate) fn setup_data_and_log_dirs(
	ldk_dir: PathBuf, _test_name: &str,
) -> (String, TempDir, String) {
	let ldk_data_dir_binding =
		Builder::new().prefix("ldk-data-dir").tempdir_in(ldk_dir.clone()).unwrap();
	let ldk_data_dir = String::from(ldk_data_dir_binding.path().to_str().unwrap());

	// Create the ldk-logs dir, which we'll persist after the tests are over for debugging.
	let ldk_log_dir_binding = ldk_dir.join(format!("ldk-logs"));
	let ldk_log_dir = String::from(ldk_log_dir_binding.as_path().to_str().unwrap());

	// Initialize the LDK data directory if necessary.
	fs::create_dir_all(ldk_log_dir.clone()).unwrap();

	(ldk_data_dir, ldk_data_dir_binding, ldk_log_dir)
}
