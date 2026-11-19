mod rtcp_fwd;

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use rtc::interceptor::{Registry, Slot};
use rtc::media_stream::MediaStreamTrack;
use rtc::peer_connection::configuration::interceptor_registry::register_default_interceptors;
use rtc::peer_connection::configuration::media_engine::MediaEngine;
use rtc::peer_connection::configuration::setting_engine::SettingEngineBuilder;
use rtc::peer_connection::configuration::RTCConfigurationBuilder;
use rtc::peer_connection::sdp::RTCSessionDescription;
use rtc::peer_connection::transport::RTCIceCandidateType;
use rtc::rtcp::payload_feedbacks::full_intra_request::FullIntraRequest;
use rtc::rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication;
use rtc::rtp;
use rtc::rtp_transceiver::rtp_sender::{
    RTCRtpCodec, RTCRtpCodingParameters, RTCRtpEncodingParameters, RtpCodecKind,
};
use rtc::rtp_transceiver::{RTCRtpTransceiverDirection, RTCRtpTransceiverInit};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc, Mutex};
use tokio_tungstenite::tungstenite::Message;
use webrtc::media_stream::track_local::static_rtp::TrackLocalStaticRTP;
use webrtc::media_stream::track_local::{TrackLocal, TrackLocalEvent};
use webrtc::media_stream::track_remote::{TrackRemote, TrackRemoteEvent};
use webrtc::peer_connection::{
    PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCIceGatheringState,
    RTCPeerConnectionState,
};
use webrtc::rtp_transceiver::RtpSender;

type Pc = Arc<dyn PeerConnection>;

struct Config {
    signal_url: String,
    public_ip: Option<String>,
    ports: Option<(u16, u16)>,
}

struct PortPool {
    free: Vec<u16>,
}

impl PortPool {
    fn take(&mut self) -> u16 {
        self.free.pop().unwrap_or(0)
    }
    fn give(&mut self, port: u16) {
        if port != 0 {
            self.free.push(port);
        }
    }
}

struct Pub {
    key: String,
    owner: String,
    kind: RtpCodecKind,
    codec: RTCRtpCodec,
    media_ssrc: u32,
    remote: Arc<dyn TrackRemote>,
    tx: broadcast::Sender<rtp::Packet>,
    last_pli: StdMutex<Instant>,
}

impl Pub {
    async fn keyframe(&self) {
        self.request_keyframe(false).await;
    }

    async fn request_keyframe(&self, force: bool) {
        if self.kind != RtpCodecKind::Video {
            return;
        }
        {
            let mut last = self.last_pli.lock().unwrap();
            if !force && last.elapsed() < Duration::from_millis(300) {
                return;
            }
            *last = Instant::now();
        }
        let _ = self
            .remote
            .write_rtcp(vec![Box::new(PictureLossIndication {
                sender_ssrc: 0,
                media_ssrc: self.media_ssrc,
            })])
            .await;
    }
}

struct Sub {
    sender: Arc<dyn RtpSender>,
    stop: Arc<AtomicBool>,
}

#[derive(Default)]
struct Down {
    pc: Option<Pc>,
    port: u16,
    subs: HashMap<String, Sub>,
    negotiating: bool,
    dirty: bool,
}

struct Client {
    id: String,
    room: String,
    up: Mutex<Option<(Pc, u16)>>,
    down: Mutex<Down>,
    pubs: Mutex<Vec<Arc<Pub>>>,
}

struct Hub {
    cfg: Config,
    out: mpsc::UnboundedSender<Value>,
    rooms: Mutex<HashMap<String, HashMap<String, Arc<Client>>>>,
    ports: StdMutex<PortPool>,
}

struct Handler {
    hub: Arc<Hub>,
    client: Option<Arc<Client>>,
    gathered: mpsc::UnboundedSender<()>,
    label: &'static str,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            let _ = self.gathered.send(());
        }
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        if let Some(c) = &self.client {
            log::info!("{} {} {}: {state}", c.room, c.id, self.label);
            if state == RTCPeerConnectionState::Failed && self.label == "up" {
                let hub = self.hub.clone();
                let c = c.clone();
                tokio::spawn(async move { hub.close_up(&c).await });
            }
        }
    }

    async fn on_track(&self, track: Arc<dyn TrackRemote>) {
        let Some(client) = self.client.clone() else { return };
        let hub = self.hub.clone();
        tokio::spawn(async move {
            if let Err(err) = hub.publish(client, track).await {
                log::warn!("publish failed: {err:#}");
            }
        });
    }
}

impl Hub {
    fn send(&self, msg: Value) {
        let _ = self.out.send(msg);
    }

    async fn new_pc(
        &self,
        client: Option<Arc<Client>>,
        label: &'static str,
        port: u16,
    ) -> Result<(Pc, mpsc::UnboundedReceiver<()>)> {
        let mut media = MediaEngine::default();
        media.register_default_codecs()?;
        let registry = register_default_interceptors(Registry::new(), &mut media)?
            .with(Slot::from(14_000), rtcp_fwd::RtcpForwarder::default());
        let mut settings = SettingEngineBuilder::new()
            .with_multicast_dns_mode(rtc::ice::mdns::MulticastDnsMode::Disabled);
        if let Some(ip) = &self.cfg.public_ip {
            settings = settings.with_nat_1to1_ips(vec![ip.clone()], RTCIceCandidateType::Host);
        }
        let (gtx, grx) = mpsc::unbounded_channel();
        let handler = Arc::new(Handler {
            hub: HUB.get().unwrap().clone(),
            client,
            gathered: gtx,
            label,
        });
        let addr = format!("0.0.0.0:{port}");
        let pc = PeerConnectionBuilder::new()
            .with_configuration(RTCConfigurationBuilder::new().build())
            .with_setting_engine(settings.build())
            .with_media_engine(media)
            .with_interceptor_registry(registry)
            .with_handler(handler as Arc<dyn PeerConnectionEventHandler>)
            .with_udp_addrs(vec![addr.clone()])
            .with_tcp_addrs(if port == 0 { vec![] } else { vec![addr] })
            .build()
            .await?;
        Ok((Arc::new(pc), grx))
    }

    fn take_port(&self) -> u16 {
        self.ports.lock().unwrap().take()
    }

    fn give_port(&self, port: u16) {
        self.ports.lock().unwrap().give(port);
    }

    async fn client(&self, room: &str, id: &str) -> Arc<Client> {
        let mut rooms = self.rooms.lock().await;
        rooms
            .entry(room.to_owned())
            .or_default()
            .entry(id.to_owned())
            .or_insert_with(|| {
                Arc::new(Client {
                    id: id.to_owned(),
                    room: room.to_owned(),
                    up: Mutex::new(None),
                    down: Mutex::new(Down::default()),
                    pubs: Mutex::new(Vec::new()),
                })
            })
            .clone()
    }

    async fn others(&self, c: &Client) -> Vec<Arc<Client>> {
        let rooms = self.rooms.lock().await;
        rooms
            .get(&c.room)
            .map(|r| r.values().filter(|o| o.id != c.id).cloned().collect())
            .unwrap_or_default()
    }

    async fn join(&self, room: &str, id: &str) {
        let c = self.client(room, id).await;
        let mut added = false;
        for other in self.others(&c).await {
            for p in other.pubs.lock().await.iter() {
                added |= self.subscribe(&c, p.clone()).await;
            }
        }
        if added {
            self.renegotiate(c).await;
        }
    }

    async fn up_offer(&self, room: &str, id: &str, sdp: Value) -> Result<()> {
        let c = self.client(room, id).await;
        self.close_up(&c).await;
        let offer: RTCSessionDescription = serde_json::from_value(sdp)?;
        let port = self.take_port();
        let (pc, mut gathered) = self.new_pc(Some(c.clone()), "up", port).await?;
        for kind in [RtpCodecKind::Video, RtpCodecKind::Audio] {
            pc.add_transceiver_from_kind(
                kind,
                Some(RTCRtpTransceiverInit {
                    direction: RTCRtpTransceiverDirection::Recvonly,
                    ..Default::default()
                }),
            )
            .await?;
        }
        pc.set_remote_description(offer).await?;
        let answer = pc.create_answer(None).await?;
        pc.set_local_description(answer).await?;
        let _ = tokio::time::timeout(Duration::from_secs(3), gathered.recv()).await;
        let local = pc.local_description().await.context("no local description")?;
        *c.up.lock().await = Some((pc, port));
        self.send(json!({ "t": "up-answer", "room": room, "id": id, "sdp": local }));
        Ok(())
    }

    async fn close_up(&self, c: &Arc<Client>) {
        if let Some((pc, port)) = c.up.lock().await.take() {
            let _ = pc.close().await;
            self.give_port(port);
        }
        let pubs: Vec<_> = c.pubs.lock().await.drain(..).collect();
        if pubs.is_empty() {
            return;
        }
        for other in self.others(c).await {
            let mut changed = false;
            {
                let mut down = other.down.lock().await;
                for p in &pubs {
                    if let Some(sub) = down.subs.remove(&p.key) {
                        sub.stop.store(true, Ordering::Relaxed);
                        if let Some(pc) = &down.pc {
                            let _ = pc.remove_track(&sub.sender).await;
                        }
                        changed = true;
                    }
                }
            }
            if changed {
                self.renegotiate(other).await;
            }
        }
    }

    async fn publish(&self, c: Arc<Client>, track: Arc<dyn TrackRemote>) -> Result<()> {
        let media_ssrc = *track.ssrcs().await.first().context("no ssrc")?;
        let codec = track.codec(media_ssrc).await.context("no codec")?;
        let kind = track.kind().await;
        let (tx, _) = broadcast::channel(1024);
        let p = Arc::new(Pub {
            key: format!("{}-{}", c.id, rand::random::<u32>()),
            owner: c.id.clone(),
            kind,
            codec: codec.clone(),
            media_ssrc,
            remote: track.clone(),
            tx: tx.clone(),
            last_pli: StdMutex::new(Instant::now() - Duration::from_secs(5)),
        });
        log::info!("{} {} publishes {}", c.room, c.id, codec.mime_type);
        c.pubs.lock().await.push(p.clone());

        for other in self.others(&c).await {
            if self.subscribe(&other, p.clone()).await {
                self.renegotiate(other).await;
            }
        }

        let pli = p.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(8)).await;
                if Arc::strong_count(&pli) <= 2 && pli.tx.receiver_count() == 0 {
                    continue;
                }
                if pli.remote.write_rtcp(vec![Box::new(PictureLossIndication { sender_ssrc: 0, media_ssrc })]).await.is_err() {
                    break;
                }
            }
        });

        while let Some(evt) = track.poll().await {
            if let TrackRemoteEvent::OnRtpPacket(pkt) = evt {
                let _ = tx.send(pkt);
            }
        }
        log::info!("{} {} track ended", c.room, c.id);
        Ok(())
    }

    async fn subscribe(&self, c: &Arc<Client>, p: Arc<Pub>) -> bool {
        let mut down = c.down.lock().await;
        if down.subs.contains_key(&p.key) {
            return false;
        }
        if down.pc.is_none() {
            let port = self.take_port();
            match self.new_pc(Some(c.clone()), "down", port).await {
                Ok((pc, mut gathered)) => {
                    down.pc = Some(pc);
                    down.port = port;
                    tokio::spawn(async move {
                        let _ = gathered.recv().await;
                    });
                }
                Err(err) => {
                    log::warn!("down pc: {err:#}");
                    self.give_port(port);
                    return false;
                }
            }
        }
        let pc = down.pc.clone().unwrap();
        let ssrc = rand::random::<u32>();
        let local = Arc::new(TrackLocalStaticRTP::new(MediaStreamTrack::new(
            p.owner.clone(),
            p.key.clone(),
            p.key.clone(),
            p.kind,
            vec![RTCRtpEncodingParameters {
                rtp_coding_parameters: RTCRtpCodingParameters { ssrc: Some(ssrc), ..Default::default() },
                codec: p.codec.clone(),
                ..Default::default()
            }],
        )));
        let sender = match pc.add_track(local.clone() as Arc<dyn TrackLocal>).await {
            Ok(s) => s,
            Err(err) => {
                log::warn!("add_track: {err:#}");
                return false;
            }
        };
        let stop = Arc::new(AtomicBool::new(false));
        down.subs.insert(p.key.clone(), Sub { sender: sender.clone(), stop: stop.clone() });
        drop(down);

        let mut rx = p.tx.subscribe();
        let fwd_stop = stop.clone();
        let fwd_sender = sender.clone();
        let fwd_local = local.clone();
        let fwd_pub = p.clone();
        tokio::spawn(async move {
            let mut pt: Option<u8> = None;
            let mut kicked = false;
            loop {
                let mut pkt = match rx.recv().await {
                    Ok(pkt) => pkt,
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        fwd_pub.keyframe().await;
                        continue;
                    }
                    Err(_) => break,
                };
                if fwd_stop.load(Ordering::Relaxed) {
                    break;
                }
                if pt.is_none() {
                    pt = negotiated_pt(&fwd_sender, &fwd_pub.codec).await;
                    if pt.is_none() {
                        continue;
                    }
                }
                if !kicked {
                    kicked = true;
                    fwd_pub.request_keyframe(true).await;
                    let again = fwd_pub.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(1200)).await;
                        again.request_keyframe(true).await;
                    });
                }
                pkt.header.ssrc = ssrc;
                pkt.header.payload_type = pt.unwrap();
                pkt.header.extension = false;
                pkt.header.extensions.clear();
                pkt.header.extension_profile = 0;
                if fwd_local.write_rtp(pkt).await.is_err() {
                    pt = None;
                }
            }
        });

        let rtcp_stop = stop;
        let rtcp_pub = p;
        tokio::spawn(async move {
            while let Some(evt) = local.poll().await {
                if rtcp_stop.load(Ordering::Relaxed) {
                    break;
                }
                if let TrackLocalEvent::OnRtcpPacket(pkts) = evt {
                    if pkts.iter().any(|x| x.as_any().is::<PictureLossIndication>() || x.as_any().is::<FullIntraRequest>()) {
                        rtcp_pub.keyframe().await;
                    }
                }
            }
        });
        true
    }

    async fn renegotiate(&self, c: Arc<Client>) {
        let mut down = c.down.lock().await;
        let Some(pc) = down.pc.clone() else { return };
        if down.negotiating {
            down.dirty = true;
            return;
        }
        down.negotiating = true;
        down.dirty = false;
        drop(down);
        let result: Result<RTCSessionDescription> = async {
            let offer = pc.create_offer(None).await?;
            pc.set_local_description(offer).await?;
            for _ in 0..30 {
                if let Some(d) = pc.local_description().await {
                    if d.sdp.contains("a=candidate") {
                        return Ok(d);
                    }
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            pc.local_description().await.context("no local description")
        }
        .await;
        match result {
            Ok(desc) => self.send(json!({ "t": "down-offer", "room": c.room, "id": c.id, "sdp": desc })),
            Err(err) => {
                log::warn!("renegotiate {}: {err:#}", c.id);
                c.down.lock().await.negotiating = false;
            }
        }
    }

    async fn down_answer(&self, room: &str, id: &str, sdp: Value) -> Result<()> {
        let c = self.client(room, id).await;
        let answer: RTCSessionDescription = serde_json::from_value(sdp)?;
        let pc = c.down.lock().await.pc.clone().context("no down pc")?;
        pc.set_remote_description(answer).await?;
        let again = {
            let mut down = c.down.lock().await;
            down.negotiating = false;
            std::mem::take(&mut down.dirty)
        };
        if again {
            self.renegotiate(c).await;
        }
        Ok(())
    }

    async fn leave(&self, room: &str, id: &str) {
        let c = {
            let rooms = self.rooms.lock().await;
            rooms.get(room).and_then(|r| r.get(id).cloned())
        };
        let Some(c) = c else { return };
        self.close_up(&c).await;
        {
            let mut rooms = self.rooms.lock().await;
            if let Some(r) = rooms.get_mut(room) {
                r.remove(id);
                if r.is_empty() {
                    rooms.remove(room);
                }
            }
        }
        let mut down = c.down.lock().await;
        for (_, sub) in down.subs.drain() {
            sub.stop.store(true, Ordering::Relaxed);
        }
        if let Some(pc) = down.pc.take() {
            let _ = pc.close().await;
        }
        let port = std::mem::take(&mut down.port);
        self.give_port(port);
    }

    async fn reset(&self) {
        let all: Vec<(String, String)> = {
            let rooms = self.rooms.lock().await;
            rooms
                .iter()
                .flat_map(|(r, cs)| cs.keys().map(move |id| (r.clone(), id.clone())))
                .collect()
        };
        for (room, id) in all {
            self.leave(&room, &id).await;
        }
    }

    async fn handle(&self, msg: Value) -> Result<()> {
        let t = msg["t"].as_str().unwrap_or_default();
        let room = msg["room"].as_str().unwrap_or_default();
        let id = msg["id"].as_str().unwrap_or_default();
        match t {
            "join" => self.join(room, id).await,
            "leave" => self.leave(room, id).await,
            "up-offer" => self.up_offer(room, id, msg["sdp"].clone()).await?,
            "up-close" => {
                let c = self.client(room, id).await;
                self.close_up(&c).await;
            }
            "down-answer" => self.down_answer(room, id, msg["sdp"].clone()).await?,
            "reset" => self.reset().await,
            _ => {}
        }
        Ok(())
    }
}

async fn negotiated_pt(sender: &Arc<dyn RtpSender>, codec: &RTCRtpCodec) -> Option<u8> {
    let params = sender.get_parameters().await.ok()?;
    let codecs = &params.rtp_parameters.codecs;
    let same_mime = |c: &&rtc::rtp_transceiver::rtp_sender::RTCRtpCodecParameters| {
        c.rtp_codec.mime_type.eq_ignore_ascii_case(&codec.mime_type)
    };
    codecs
        .iter()
        .filter(same_mime)
        .find(|c| c.rtp_codec.sdp_fmtp_line == codec.sdp_fmtp_line)
        .or_else(|| codecs.iter().find(same_mime))
        .map(|c| c.payload_type)
}

static HUB: std::sync::OnceLock<Arc<Hub>> = std::sync::OnceLock::new();

async fn signaling(hub: Arc<Hub>, mut out_rx: mpsc::UnboundedReceiver<Value>) {
    loop {
        match tokio_tungstenite::connect_async(hub.cfg.signal_url.as_str()).await {
            Ok((ws, _)) => {
                log::info!("connected to signaling");
                let (mut sink, mut stream) = ws.split();
                let _ = sink.send(Message::Text(json!({ "t": "sfu-hello" }).to_string().into())).await;
                let mut ping = tokio::time::interval(Duration::from_secs(20));
                loop {
                    tokio::select! {
                        msg = stream.next() => match msg {
                            Some(Ok(Message::Text(text))) => {
                                if let Ok(value) = serde_json::from_str::<Value>(&text) {
                                    let hub = hub.clone();
                                    tokio::spawn(async move {
                                        if let Err(err) = hub.handle(value).await {
                                            log::warn!("handle: {err:#}");
                                        }
                                    });
                                }
                            }
                            Some(Ok(_)) => {}
                            _ => break,
                        },
                        Some(out) = out_rx.recv() => {
                            if sink.send(Message::Text(out.to_string().into())).await.is_err() {
                                break;
                            }
                        }
                        _ = ping.tick() => {
                            if sink.send(Message::Text(json!({ "t": "sfu-ping" }).to_string().into())).await.is_err() {
                                break;
                            }
                        }
                    }
                }
                log::warn!("signaling disconnected");
                hub.reset().await;
            }
            Err(err) => log::warn!("signaling connect failed: {err}"),
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info,webrtc::peer_connection::driver=off,rtc_mdns=off")).init();
    let server = std::env::var("SIGNAL_URL").unwrap_or_else(|_| "wss://telinha-xp0y.onrender.com/sfu".into());
    let key = std::env::var("SFU_KEY").context("SFU_KEY is required")?;
    let sep = if server.contains('?') { '&' } else { '?' };
    let ports = match (std::env::var("PORT_MIN"), std::env::var("PORT_MAX")) {
        (Ok(a), Ok(b)) => Some((a.parse()?, b.parse()?)),
        _ => None,
    };
    let cfg = Config {
        signal_url: format!("{server}{sep}key={key}"),
        public_ip: std::env::var("PUBLIC_IP").ok().filter(|s| !s.is_empty()),
        ports,
    };
    let free = cfg.ports.map(|(a, b)| (a..=b).rev().collect()).unwrap_or_default();
    let (out_tx, out_rx) = mpsc::unbounded_channel();
    let hub = Arc::new(Hub {
        cfg,
        out: out_tx,
        rooms: Mutex::new(HashMap::new()),
        ports: StdMutex::new(PortPool { free }),
    });
    let _ = HUB.set(hub.clone());
    log::info!("telinha-sfu public_ip={:?} ports={:?}", hub.cfg.public_ip, hub.cfg.ports);
    signaling(hub, out_rx).await;
    Ok(())
}
