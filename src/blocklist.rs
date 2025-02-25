use crate::{parser, trie::Trie, CLIENT, LIST_DIR};
use anyhow::Context;
use log::{error, info, warn};
use num_format::{Locale, ToFormattedString};
use std::{
	path::PathBuf,
	sync::{
		atomic::{AtomicUsize, Ordering},
		Arc
	}
};
use tokio::{
	fs::{create_dir_all, read_to_string, write},
	sync::RwLock
};
use url::Url;

#[derive(Default)]
pub(crate) struct BlockList {
	trie: RwLock<Trie>
}

impl BlockList {
	pub(crate) fn new() -> Self {
		BlockList::default()
	}

	///Clear and update the current Blocklist, to all entries of the list at from `adlist`.
	///if `use_cache` is set true, cached list, will not be redownloaded (faster init)
	pub(crate) async fn update(
		&self,
		adlist: &Vec<Url>,
		restore_from_cache: bool,
		blocklist_len: Arc<AtomicUsize>
	) {
		if restore_from_cache {
			info!("👮💾 restore blocklist, from cache");
		} else {
			info!("👮📥 updating blocklist");
		}
		if let Err(err) = create_dir_all(&*LIST_DIR)
			.await
			.with_context(|| format!("failed create dir {:?}", LIST_DIR.as_path()))
		{
			error!("{err:?}");
		}
		let mut trie = Trie::new();

		for url in adlist {
			let raw_list = if url.scheme() == "file" {
				let path = url.path();
				info!("load file {path:?}");
				let raw_list = read_to_string(&path).await;
				match raw_list.with_context(|| format!("can not open file {path:?}")) {
					Ok(value) => Some(value),
					Err(err) => {
						error!("{err:?}");
						None
					}
				}
			} else {
				let mut path = url.path().to_owned().replace('/', "-");
				if !path.is_empty() {
					path.remove(0);
				}
				if let Some(query) = url.query() {
					path += "--";
					path += query;
				}
				let path = PathBuf::from(&*LIST_DIR).join(path);
				let raw_list = if !path.exists() || !restore_from_cache {
					info!("downloading {url}");
					let resp: anyhow::Result<String> = (|| async {
						//try block
						let resp = CLIENT
							.get(url.to_owned())
							.send()
							.await?
							.error_for_status()?
							.text()
							.await?;
						if let Err(err) = write(&path, &resp)
							.await
							.with_context(|| format!("failed to save to {path:?}"))
						{
							error!("{err:?}");
						}
						Ok(resp)
					})()
					.await;
					match resp.with_context(|| format!("error downloading {url}")) {
						Ok(value) => Some(value),
						Err(err) => {
							error!("{err:?}");
							None
						}
					}
				} else {
					None
				};
				match raw_list {
					Some(value) => Some(value),
					None => {
						if path.exists() {
							info!("restore from cache {url}");
							match read_to_string(&path)
								.await
								.with_context(|| format!("error reading file {path:?}"))
							{
								Ok(value) => Some(value),
								Err(err) => {
									error!("{err:?}");
									None
								}
							}
						} else {
							None
						}
					},
				}
			};
			match raw_list {
				None => error!("skipp list {url}"),
				Some(raw_list) => {
					let result = parser::Blocklist::parse(url.as_str(), &raw_list);
					match result {
						Err(err) => {
							error!("parsing Blockist {}", url.as_str());
							err.print();
						},
						Ok(list) => {
							for entry in list.entries {
								trie.insert(&entry.domain().0);
							}
						},
					}
				}
			}
		}
		info!("shrink blocklist");
		trie.shrink_to_fit();
		let blocked_count = trie.len();
		blocklist_len.store(blocked_count, Ordering::Relaxed);
		info!(
			"{} domains are blocked",
			blocked_count.to_formatted_string(&Locale::en)
		);
		if blocked_count == 0 {
			warn!("Blocklist is empty");
		}
		let mut guard = self.trie.write().await;
		*guard = trie;
		drop(guard);
		info!("👮✅ finish updating blocklist");
	}

	pub(crate) async fn contains(&self, domain: &str, include_subdomains: bool) -> bool {
		self.trie.read().await.contains(domain, include_subdomains)
	}
}
