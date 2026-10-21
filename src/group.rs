//! Pairwise E2E channels over a routed room. Each speaker has their own Opus
//! decoder/jitter queue; only decoded PCM is mixed, once per 20 ms.
use crate::{
    audio::{AudioEngine, Decoder, FRAME_SAMPLES},
    config::Config,
    net::{NetCmd, NetEvent},
    proto::{AUDIO_PAYLOAD_MAX, Cipher, Packet},
    transfer::{self, RoutedOutput, RoutedWorker},
};
use remember_me::group_relay as wire;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{net::UdpSocket, sync::mpsc};
use x25519_dalek::{PublicKey, StaticSecret};

#[derive(Clone, Debug)]
pub struct Participant {
    pub id: u64,
    pub name: String,
    pub rtt: Option<u16>,
    pub mute: bool,
    pub deaf: bool,
    pub speaking: bool,
    pub level: u8,
    pub known: bool,
    pub avatar: Arc<crate::avatar::Avatar>,
}
struct Peer {
    identity: wire::Identity,
    name_delivery: crate::avatar::Delivery,
    name_received: u64,
    avatar_custom: bool,
    avatar_delivery: crate::avatar::Delivery,
    avatar_received: u64,
    avatar_assembly: crate::avatar::Assembly,
    cipher: Cipher,
    control: u64,
    sequence: u32,
    decoder: Decoder,
    audio: VecDeque<f32>,
    buffering: bool,
    last_audio: Instant,
    last_authenticated: Instant,
    status: Participant,
    files: RoutedWorker,
    seen_text: VecDeque<u32>,
    pings: VecDeque<(u64, Instant)>,
}
struct Pending {
    id: u32,
    text: String,
    waiting: BTreeSet<u64>,
    sent: Instant,
    tries: u32,
}
struct Membership {
    socket: Arc<UdpSocket>,
    server: std::net::SocketAddr,
    code: wire::RoomCode,
    id: u64,
}
impl Drop for Membership {
    fn drop(&mut self) {
        let _ = self
            .socket
            .try_send_to(&wire::leave(self.code, self.id), self.server);
    }
}
fn prefixes(a: &wire::Identity, b: &wire::Identity, epoch: u64, code: wire::RoomCode) -> [u8; 32] {
    let (a, b) = if a.id < b.id { (a, b) } else { (b, a) };
    let mut hash = Sha256::new();
    hash.update(b"remember-me routed room v1");
    hash.update(code);
    hash.update(epoch.to_be_bytes());
    for member in [a, b] {
        hash.update(member.id.to_be_bytes());
        hash.update(member.generation.to_be_bytes());
        hash.update(member.key);
    }
    hash.finalize().into()
}
async fn send(
    socket: &UdpSocket,
    server: std::net::SocketAddr,
    own: u64,
    peer: &mut Peer,
    packet: Packet,
) {
    let encrypted = Packet::encrypt_wire(&mut peer.cipher, &packet);
    let _ = socket
        .send_to(&wire::data(own, peer.identity.id, &encrypted), server)
        .await;
}
fn trace(message: &str) {
    if std::env::var("RM_DEBUG").ok().as_deref() == Some("1") {
        eprintln!("[group] {message}");
    }
}

pub async fn run(
    cfg: Config,
    code: wire::RoomCode,
    target: String,
    events: mpsc::UnboundedSender<NetEvent>,
    mut commands: mpsc::UnboundedReceiver<NetCmd>,
    mut media: mpsc::Receiver<Vec<u8>>,
    engine: Arc<AudioEngine>,
) {
    if let Err(error) = session(
        cfg,
        code,
        target,
        &events,
        &mut commands,
        &mut media,
        engine,
    )
    .await
    {
        let _ = events.send(NetEvent::Fatal(error));
    }
}
async fn session(
    cfg: Config,
    code: wire::RoomCode,
    target: String,
    events: &mpsc::UnboundedSender<NetEvent>,
    commands: &mut mpsc::UnboundedReceiver<NetCmd>,
    media: &mut mpsc::Receiver<Vec<u8>>,
    engine: Arc<AudioEngine>,
) -> Result<(), String> {
    let server = tokio::net::lookup_host(&target)
        .await
        .map_err(|e| format!("адрес сервера: {e}"))?
        .find(std::net::SocketAddr::is_ipv4)
        .ok_or("нужен IPv4 сервера")?;
    let socket = Arc::new(
        UdpSocket::bind("0.0.0.0:0")
            .await
            .map_err(|e| e.to_string())?,
    );
    let secret = StaticSecret::from(crate::net::rnd32());
    let own_id = u64::from_be_bytes(crate::net::rnd32()[..8].try_into().unwrap());
    let mut own = wire::Identity {
        id: own_id,
        generation: 0,
        key: PublicKey::from(&secret).to_bytes(),
        name: cfg.name.clone(),
    };
    let _membership = Membership {
        socket: socket.clone(),
        server,
        code,
        id: own_id,
    };
    let (files_tx, mut files_rx) = mpsc::channel(256);
    let mut peers: BTreeMap<u64, Peer> = BTreeMap::new();
    let mut pending: Vec<Pending> = Vec::new();
    let mut download_dir = cfg.download_directory();
    let mut profile = crate::avatar::Profile::new(cfg.avatar.as_ref());
    let mut name = crate::profile::Name::new(&cfg.name);
    let mut roster_version = None;
    let mut entered = false;
    let started = Instant::now();
    let mut last_server = started;
    let mut last_reg = started - Duration::from_secs(3);
    let mut last_status = last_reg;
    let mut last_ping = last_reg;
    let mut last_ui = last_reg;
    let mut message_id = 0u32;
    let mut audio_seq = 0u32;
    let mut ping_id = 0u64;
    let mut tick = tokio::time::interval(Duration::from_millis(20));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut buffer = [0; 1500];
    engine.prefill_out();
    trace(&format!("код комнаты {}", String::from_utf8_lossy(&code)));
    let _ = events.send(NetEvent::RoomCode(
        String::from_utf8_lossy(&code).into_owned(),
    ));
    loop {
        tokio::select! {
            received=socket.recv_from(&mut buffer)=>{
                let (n,from)=received.map_err(|e|e.to_string())?;if from!=server{continue}
                let bytes=&buffer[..n];
                if bytes.first()==Some(&wire::ERROR){return Err(if bytes.get(1)==Some(&1){format!("комната заполнена: максимум {} участников",wire::MAX_MEMBERS)}else{"конфликт участника комнаты; подключитесь заново".into()})}
                if let Some((room,epoch,revision,members))=wire::parse_roster(bytes){
                    if room!=code{continue}let Some(me)=members.iter().find(|m|m.id==own_id && m.key==own.key)else{continue};
                    last_server=Instant::now();
                    if roster_version.is_some_and(|(old_epoch,old_revision,_)|old_epoch==epoch&&old_revision>revision){continue}
                    if roster_version==Some((epoch,revision,me.generation)){continue}
                    let reset=roster_version.is_some_and(|(old_epoch,_,old_generation)|old_epoch!=epoch||old_generation!=me.generation);
                    own.generation=me.generation;
                    peers.retain(|id,p|!reset && members.iter().any(|m|m.id==*id&&m.generation==p.identity.generation&&m.key==p.identity.key));
                    for identity in members.into_iter().filter(|m|m.id!=own_id){
                        if peers.contains_key(&identity.id){continue}
                        let shared=secret.diffie_hellman(&PublicKey::from(identity.key)).to_bytes();if shared==[0;32]{continue}
                        let salt=prefixes(&own,&identity,epoch,code);let id=identity.id;
                        peers.insert(id,Peer{status:Participant{id,name:identity.name.clone(),rtt:None,mute:false,deaf:false,speaking:false,level:0,known:false,avatar:crate::avatar::display(None,&identity.name)},name_delivery:Default::default(),name_received:0,avatar_custom:false,avatar_delivery:Default::default(),avatar_received:0,avatar_assembly:Default::default(),cipher:Cipher::new(&shared,&salt,own_id<id),identity,control:0,sequence:0,decoder:Decoder::new()?,audio:VecDeque::new(),buffering:true,last_audio:Instant::now()-Duration::from_secs(10),last_authenticated:Instant::now()-Duration::from_secs(10),files:transfer::spawn_routed(download_dir.clone(),id,files_tx.clone()),seen_text:VecDeque::new(),pings:VecDeque::new()});
                    }
                    roster_version=Some((epoch,revision,own.generation));
                    if !entered{entered=true;let _=events.send(NetEvent::GroupEntered{room:String::from_utf8_lossy(&code).into_owned(),server:target.clone()});trace("joined room");}
                    last_status=Instant::now()-Duration::from_secs(2);last_ping=last_status;last_ui=last_status;
                    trace(&format!("roster: {} remote members",peers.len()));
                }else if let Some((from,to,body))=wire::parse_data(bytes){
                    if to!=own_id{continue}let Some(peer)=peers.get_mut(&from)else{continue};
                    let previous=peer.sequence;
                    let Some(packet)=Packet::decrypt_wire(&peer.cipher,body,&mut peer.control,&mut peer.sequence)else{continue};
                    peer.last_authenticated=Instant::now();
                    match packet {
                        Packet::Audio{payload,..} if payload.len()<=AUDIO_PAYLOAD_MAX=>{
                            let missing=if previous==0{0}else{peer.sequence.wrapping_sub(previous).saturating_sub(1).min(3)};
                            if let Ok(decoded)=peer.decoder.decode_with_loss(&payload,missing){peer.audio.extend(decoded);while peer.audio.len()>FRAME_SAMPLES*10{peer.audio.drain(..FRAME_SAMPLES);}}
                            peer.last_audio=Instant::now();peer.status.speaking=true;
                        }
                        Packet::Name{revision,name}=>{
                            send(&socket,server,own_id,peer,Packet::NameAck{revision}).await;
                            if revision>peer.name_received {
                                peer.name_received=revision;peer.identity.name=name.clone();peer.status.name=name.clone();
                                if !peer.avatar_custom {peer.status.avatar=crate::avatar::display(None,&name);}
                                trace(&format!("name received: {name} revision={revision}"));
                                last_ui=Instant::now()-Duration::from_secs(1);
                            }
                        }
                        Packet::NameAck{revision}=>peer.name_delivery.ack(revision),
                        Packet::Avatar{revision,avatar}=>{send(&socket,server,own_id,peer,Packet::AvatarAck{revision}).await;if revision>peer.avatar_received{peer.avatar_received=revision;peer.avatar_custom=avatar.is_some();peer.status.avatar=avatar.unwrap_or_else(||crate::avatar::display(None,&peer.identity.name));trace(&format!("avatar received from {} revision={revision}",peer.identity.name));}}
                        Packet::AvatarChunk{revision,index,count,bytes}=>{
                            if revision<=peer.avatar_received {send(&socket,server,own_id,peer,Packet::AvatarAck{revision}).await;}
                            else if let Some(avatar)=peer.avatar_assembly.push(revision,index,count,bytes){
                                peer.avatar_received=revision;peer.avatar_custom=true;peer.status.avatar=avatar;
                                send(&socket,server,own_id,peer,Packet::AvatarAck{revision}).await;
                                trace(&format!("avatar received from {} revision={revision}",peer.identity.name));
                            }
                        }
                        Packet::AvatarAck{revision}=>peer.avatar_delivery.ack(revision),
                        Packet::Ping{msg_id,ts_ms}=>send(&socket,server,own_id,peer,Packet::Pong{msg_id,ts_ms}).await,
                        Packet::Pong{msg_id,..}=>{if let Some(index)=peer.pings.iter().position(|(id,_)|*id==msg_id){let (_,sent)=peer.pings.remove(index).unwrap();peer.status.rtt=Some(sent.elapsed().as_millis().min(u16::MAX as u128) as u16);}}
                        Packet::Status{mute_mic,deaf,lvl}=>{peer.status.mute=mute_mic;peer.status.deaf=deaf;peer.status.level=lvl;peer.status.known=true;}
                        Packet::Text{msg_id,text}=>{
                            send(&socket,server,own_id,peer,Packet::Ack{msg_id}).await;
                            if !peer.seen_text.contains(&msg_id){peer.seen_text.push_back(msg_id);if peer.seen_text.len()>256{peer.seen_text.pop_front();}let _=events.send(NetEvent::GroupText{name:peer.identity.name.clone(),text});}
                        }
                        Packet::Ack{msg_id}=>{if let Some(message)=pending.iter_mut().find(|m|m.id==msg_id){message.waiting.remove(&from);if message.waiting.is_empty(){let _=events.send(NetEvent::TextAck(msg_id));}}pending.retain(|m|!m.waiting.is_empty());}
                        Packet::File(packet)=>{let _=peer.files.commands.try_send(transfer::Command::Wire(packet));}
                        Packet::Bye{..}=>{peer.status.known=false;peer.status.speaking=false;peer.audio.clear();}
                        _=>{}
                    }
                }
            }
            cmd=commands.recv()=>match cmd {
                Some(NetCmd::Stop|NetCmd::HangUp{..})|None=>{let _=events.send(NetEvent::CallEnded("вы вышли из комнаты".into()));return Ok(())}
                Some(NetCmd::Name(value))=>{name.set(&value);own.name=name.value.clone();}
                Some(NetCmd::Avatar(avatar))=>profile.set(avatar),
                Some(NetCmd::DownloadDirectory(path))=>{download_dir=path;for p in peers.values(){p.files.set_directory(download_dir.clone());}}
                Some(NetCmd::SendFile(path))=>{if peers.is_empty(){let _=events.send(NetEvent::Progress("в комнате пока нет получателей файла".into()));}for p in peers.values(){let _=p.files.commands.try_send(transfer::Command::Send(path.clone()));}}
                Some(NetCmd::FileDecision{id,peer_id,accept})=>{if let Some(p)=peer_id.and_then(|id|peers.get(&id)){let _=p.files.commands.try_send(if accept {transfer::Command::Accept(id)} else {transfer::Command::Decline(id)});}}
                Some(NetCmd::CancelFile(id))=>{for p in peers.values(){let _=p.files.commands.try_send(transfer::Command::Cancel(id));}}
                Some(NetCmd::Text(text))=>{message_id=message_id.wrapping_add(1);let mut waiting=BTreeSet::new();for (id,p) in &mut peers{send(&socket,server,own_id,p,Packet::Text{msg_id:message_id,text:text.clone()}).await;waiting.insert(*id);}if waiting.is_empty(){let _=events.send(NetEvent::TextAck(message_id));}else{pending.push(Pending{id:message_id,text,waiting,sent:Instant::now(),tries:0});}}
                _=>{}
            },
            Some(frame)=media.recv()=>{audio_seq=audio_seq.wrapping_add(1);for p in peers.values_mut(){send(&socket,server,own_id,p,Packet::Audio{ssrc:own_id as u32,seq:audio_seq,payload:frame.clone()}).await;}},
            Some((id,output))=files_rx.recv()=>{if let Some(p)=peers.get_mut(&id){match output{RoutedOutput::Packet(packet)=>send(&socket,server,own_id,p,Packet::File(packet)).await,RoutedOutput::Update(mut update)=>{update.peer_id=Some(id);update.peer_name=Some(p.identity.name.clone());let _=events.send(NetEvent::Transfer(update));}}}},
            _=tick.tick()=>{
                let now=Instant::now();
                if last_reg.elapsed()>=Duration::from_secs(2){last_reg=now;socket.send_to(&wire::registration(code,&own),server).await.map_err(|e|e.to_string())?;}
                if !entered&&started.elapsed()>Duration::from_secs(15){return Err("нет ответа группового сервера: обновите remember-me-server на VPS и проверьте UDP-порт".into())}
                let mut mixed=[0f32;FRAME_SAMPLES];
                for p in peers.values_mut(){
                    if p.last_audio.elapsed()>Duration::from_millis(400){p.status.speaking=false;}
                    if p.last_authenticated.elapsed()>Duration::from_secs(12){p.status.known=false;p.status.speaking=false;}
                    if p.buffering&&p.audio.len()>=FRAME_SAMPLES*2{p.buffering=false;}
                    if !p.buffering&&p.audio.len()>=FRAME_SAMPLES {for sample in &mut mixed{*sample+=p.audio.pop_front().unwrap();}}else{p.buffering=true;}
                }
                engine.push_out(&mixed);
                for p in peers.values_mut(){
                    if let Some(packet)=name.next(&mut p.name_delivery,now){send(&socket,server,own_id,p,packet).await;}
                    if let Some(packet)=profile.next(&mut p.avatar_delivery,now){send(&socket,server,own_id,p,packet).await;}}
                if last_status.elapsed()>=Duration::from_secs(1){last_status=now;for p in peers.values_mut(){send(&socket,server,own_id,p,Packet::Status{mute_mic:engine.params.mute_mic(),deaf:engine.params.deaf(),lvl:engine.params.in_level()}).await;}}
                if last_ping.elapsed()>=Duration::from_secs(2){last_ping=now;ping_id+=1;for p in peers.values_mut(){p.pings.push_back((ping_id,now));while p.pings.len()>8{p.pings.pop_front();}send(&socket,server,own_id,p,Packet::Ping{msg_id:ping_id,ts_ms:0}).await;}}
                for message in &mut pending{if message.sent.elapsed()>Duration::from_millis(1500){message.tries+=1;message.sent=now;for id in &message.waiting{if let Some(p)=peers.get_mut(id){send(&socket,server,own_id,p,Packet::Text{msg_id:message.id,text:message.text.clone()}).await;}}}}
                pending.retain(|m|m.tries<4);
                if last_ui.elapsed()>Duration::from_millis(200){last_ui=now;let _=events.send(NetEvent::GroupRoster(peers.values().map(|p|p.status.clone()).collect()));}
                if entered&&last_server.elapsed()>Duration::from_secs(10){let _=events.send(NetEvent::Progress("сервер временно недоступен, восстанавливаем комнату…".into()));last_server=now;}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn membership_generation_changes_the_nonce_domain_but_roster_order_does_not() {
        let a = wire::Identity {
            id: 1,
            generation: 1,
            key: [1; 32],
            name: "a".into(),
        };
        let mut b = wire::Identity {
            id: 2,
            generation: 2,
            key: [2; 32],
            name: "b".into(),
        };
        assert_eq!(prefixes(&a, &b, 7, *b"AX3P"), prefixes(&b, &a, 7, *b"AX3P"));
        let before = prefixes(&a, &b, 7, *b"AX3P");
        b.generation += 1;
        assert_ne!(before, prefixes(&a, &b, 7, *b"AX3P"));
        assert_ne!(before, prefixes(&a, &b, 8, *b"AX3P"));
    }
}
