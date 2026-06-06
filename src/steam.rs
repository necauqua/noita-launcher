use std::collections::HashMap;
use std::io::Cursor;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;

use aes::Aes256;
use aes::cipher::BlockModeDecrypt;
use aes::cipher::KeyInit;
use aes::cipher::KeyIvInit;
use aes::cipher::block_padding::Pkcs7;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use byteorder::LittleEndian;
use byteorder::ReadBytesExt;
use color_eyre::Section;
use eyre::Context;
use eyre::OptionExt;
use eyre::Result;
use eyre::bail;
use reqwest::Client;
use steam_vent::Connection;
use steam_vent::ConnectionTrait;
use steam_vent::EResult;
use steam_vent_proto::content_manifest::ContentManifestMetadata;
use steam_vent_proto::content_manifest::ContentManifestPayload;
use steam_vent_proto::content_manifest::ContentManifestSignature;
use steam_vent_proto::protobuf::Message;
use steam_vent_proto::steammessages_clientserver_2::CMsgClientGetDepotDecryptionKey;
use steam_vent_proto::steammessages_clientserver_2::CMsgClientGetDepotDecryptionKeyResponse;
use steam_vent_proto::steammessages_clientserver_appinfo::CMsgClientPICSProductInfoRequest;
use steam_vent_proto::steammessages_clientserver_appinfo::CMsgClientPICSProductInfoResponse;
use steam_vent_proto::steammessages_clientserver_appinfo::cmsg_client_picsproduct_info_request::AppInfo;
use steam_vent_proto::steammessages_contentsystem_steamclient::CContentServerDirectory_GetManifestRequestCode_Request;
use steam_vent_proto::steammessages_contentsystem_steamclient::CContentServerDirectory_GetServersForSteamPipe_Request;

use crate::cdn::CdnDownloader;

pub struct Steam {
    conn: Connection,
    cdn_servers: Option<Arc<[String]>>,
    depot_cache: HashMap<(u32, u32), Depot>,
    http: reqwest::Client,
    manifest_cache: PathBuf,
}

#[derive(Debug)]
pub struct Branch {
    pub name: String,
    pub manifest: u64,
    pub description: Option<String>,
}

#[derive(Debug)]
pub struct CurrentRelease {
    pub branches: Vec<Branch>,
}

#[derive(Debug, thiserror::Error)]
#[error("access denied")]
pub struct AccessDenied;

impl Steam {
    pub fn new(conn: Connection, manifest_cache: PathBuf) -> Self {
        Self {
            conn,
            cdn_servers: None,
            depot_cache: Default::default(),
            http: Client::new(),
            manifest_cache,
        }
    }

    pub async fn get_current_release(&self, app_id: u32, depot_id: u32) -> Result<CurrentRelease> {
        let mut req = CMsgClientPICSProductInfoRequest::new();
        req.apps.push({
            let mut app = AppInfo::new();
            app.set_appid(app_id);
            app
        });
        req.set_meta_data_only(false);

        let resp: CMsgClientPICSProductInfoResponse = self.conn.job(req).await?;

        let vdf = std::str::from_utf8(
            resp.apps
                .first()
                .expect("PICSProductInfoResponse contained no apps")
                .buffer(),
        )
        .wrap_err("PICSProductInfoResponse.buffer was not valid utf-8")?;

        let vdf = steam_vdf_parser::parse_text(vdf)
            .wrap_err("PICSProductInfoResponse.buffer was not valid vdf")?;

        let branches = vdf
            .get_obj(&["depots", "branches"])
            .ok_or_eyre("No depots->branches in app info")?;

        let manifests = vdf
            .get_obj(&["depots", &depot_id.to_string(), "manifests"])
            .ok_or_eyre("Depot not found in app info")?;

        let branches = branches.iter().try_fold(Vec::new(), |mut acc, (name, v)| {
            acc.push(Branch {
                name: (**name).to_owned(),
                manifest: manifests
                    .get(name)
                    .and_then(|v| v.get_str(&["gid"]))
                    .and_then(|m| m.parse().ok())
                    .ok_or_eyre("Branch manifest missing or invalid")?,
                description: v.get_str(&["description"]).map(|s| s.to_owned()),
            });
            eyre::Ok(acc)
        })?;

        Ok(CurrentRelease { branches })
    }

    pub async fn get_depot(&mut self, app_id: u32, depot_id: u32) -> Result<Depot> {
        if let Some(depot) = self.depot_cache.get(&(app_id, depot_id)) {
            return Ok(depot.clone());
        }

        let mut req = CMsgClientGetDepotDecryptionKey::new();
        req.set_app_id(app_id);
        req.set_depot_id(depot_id);

        let resp: CMsgClientGetDepotDecryptionKeyResponse = self.conn.job(req).await?;

        let res = EResult::try_from(resp.eresult()).unwrap_or(EResult::Invalid);
        if !matches!(res, EResult::OK) {
            bail!("Failed to get depot decryption key: {res:?}");
        }

        let depot = Depot {
            id: depot_id,
            key: resp
                .depot_encryption_key()
                .try_into()
                .wrap_err("Unexpected depot key length")?,
        };

        self.depot_cache.insert((app_id, depot_id), depot.clone());

        Ok(depot)
    }

    pub async fn get_cdn_hosts(&mut self) -> Result<Arc<[String]>> {
        if let Some(servers) = &self.cdn_servers {
            return Ok(servers.clone());
        }

        let mut req = CContentServerDirectory_GetServersForSteamPipe_Request::new();
        req.set_cell_id(self.conn.cell_id());
        // req.set_max_servers(20);

        let resp = self.conn.service_method(req).await?;
        let servers: Arc<[_]> = resp
            .servers
            .into_iter()
            .filter_map(|mut s| {
                (s.type_() == "SteamCache")
                    .then_some(s.host.take())
                    .flatten()
            })
            .collect::<Vec<_>>()
            .into();

        self.cdn_servers = Some(servers.clone());

        Ok(servers)
    }

    fn parse_manifest(
        depot: &Depot,
        bytes: &[u8],
    ) -> eyre::Result<(ContentManifestMetadata, ContentManifestPayload)> {
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes))?;
        let mut entry = zip.by_index(0)?;
        let mut raw = Vec::with_capacity(entry.size() as _);
        entry.read_to_end(&mut raw)?;
        drop(entry);

        let mut cur = Cursor::new(&raw[..]);

        fn read_section<T: Message>(cur: &mut Cursor<&[u8]>) -> Result<T> {
            let len = cur.read_u32::<LittleEndian>()? as usize;
            let start = cur.position() as usize;
            let end = start + len;
            let bytes = cur
                .get_ref()
                .get(start..end)
                .ok_or_eyre("Manifest section length out of bounds")?;
            cur.set_position(end as u64);
            Ok(T::parse_from_bytes(bytes)?)
        }

        let mut meta = None;
        let mut payload = None;
        let mut signature = None;

        loop {
            match cur.read_u32::<LittleEndian>()? {
                0x1F4812BE => meta = Some(read_section::<ContentManifestMetadata>(&mut cur)?),
                0x71F617D0 => payload = Some(read_section::<ContentManifestPayload>(&mut cur)?),
                0x1B81B817 => signature = Some(read_section::<ContentManifestSignature>(&mut cur)?),
                0x32C415AB => break,
                other => bail!("Unknown manifest section {other:#x}"),
            }
        }

        let meta = meta.ok_or_eyre("Manifest missing metadata")?;
        let mut payload = payload.ok_or_eyre("Manifest missing payload")?;
        let _signature = signature.ok_or_eyre("Manifest missing signature")?;
        // ^ todo verify signature

        if meta.filenames_encrypted() {
            for file in &mut payload.mappings {
                file.set_filename(depot.decrypt_path(file.filename())?);
                if !file.linktarget().is_empty() {
                    file.set_linktarget(depot.decrypt_path(file.linktarget())?);
                }
            }
        }

        Ok((meta, payload))
    }

    async fn get_manifest_request_code(
        &self,
        app_id: u32,
        depot_id: u32,
        manifest: u64,
        branch: Option<String>,
    ) -> Result<u64> {
        let mut req = CContentServerDirectory_GetManifestRequestCode_Request::new();
        req.set_app_id(app_id);
        req.set_depot_id(depot_id);
        req.set_manifest_id(manifest);
        req.app_branch = branch;

        let resp = self.conn.service_method(req).await?;

        let request_code: u64 = resp.manifest_request_code();
        if request_code == 0 {
            bail!("Did not receive a manifest request code");
        }
        Ok(request_code)
    }

    pub async fn fetch_manifest(
        &mut self,
        app_id: u32,
        depot: &Depot,
        manifest: u64,
        branch: Option<String>,
    ) -> Result<(ContentManifestMetadata, ContentManifestPayload)> {
        let cache_entry = self.manifest_cache.join(manifest.to_string());
        tokio::fs::create_dir_all(&self.manifest_cache).await?;

        match tokio::fs::read(&cache_entry).await {
            Ok(bytes) => {
                tracing::debug!("manifest {manifest} found in cache");

                return match Self::parse_manifest(depot, &bytes) {
                    Ok(d) => Ok(d),
                    e => match tokio::fs::remove_file(cache_entry).await {
                        Err(sub) if sub.kind() != std::io::ErrorKind::NotFound => e.error(sub),
                        _ => e,
                    }
                    .wrap_err("Failed to parse cached manifest"),
                };
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }

        let request_code = self
            .get_manifest_request_code(app_id, depot.id, manifest, branch)
            .await?;
        let url_path = format!("depot/{}/manifest/{manifest}/5/{request_code}", depot.id);

        CdnDownloader::new(self.get_cdn_hosts().await?, self.http.clone(), 32)
            .download(&url_path, None, |bytes| async {
                let d = Self::parse_manifest(depot, &bytes)?;
                tokio::fs::write(&cache_entry, bytes).await?;
                Ok(d)
            })
            .await
    }
}

#[derive(Clone)]
pub struct Depot {
    pub id: u32,
    key: [u8; 32],
}

impl Depot {
    pub fn decrypt<'d>(&self, data: &'d mut [u8]) -> Result<&'d [u8]> {
        let (iv, data) = data
            .split_first_chunk_mut::<16>()
            .ok_or_eyre("encrypted blob too short")?;

        let key = (&self.key).into();

        // decrypt IV with ECB
        ecb::Decryptor::<Aes256>::new(key).decrypt_block(iv.into());
        // decrypt the rest with CBC using that IV
        Ok(cbc::Decryptor::<Aes256>::new(key, (&*iv).into()).decrypt_padded::<Pkcs7>(data)?)
    }

    pub fn decrypt_path(&self, text: &str) -> Result<String> {
        let mut data = B64.decode(
            text.chars()
                .filter(|ch| !ch.is_whitespace()) // some base64 fields seemed to contain newlines
                .collect::<String>(),
        )?;
        let s = std::str::from_utf8(self.decrypt(&mut data)?)?;
        Ok(s.trim_end_matches('\0').replace('\\', "/"))
    }
}
