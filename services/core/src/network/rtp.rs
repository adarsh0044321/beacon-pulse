//! RTP-style packet format for Beacon/Pulse UDP stream.
//!
//! Packet layout (little-endian):
//! ┌──────────────────────────────────────────────────────┐
//! │  Magic     [4]  = 0x4C414E53 ("LANS")                │
//! │  Version   [1]  = 1                                   │
//! │  Flags     [1]  = bit0: keyframe, bit1: fragment_end  │
//! │  DisplayId [1]  = display index (e.g. 0 or 1)         │
//! │  SeqNum    [2]  = packet sequence (wraps at 65535)    │
//! │  Timestamp [8]  = microseconds since epoch            │
//! │  Width     [2]  = frame width in pixels               │
//! │  Height    [2]  = frame height in pixels              │
//! │  FragIdx   [2]  = fragment index within this frame    │
//! │  FragTotal [2]  = total fragments for this frame      │
//! │  DataLen   [2]  = payload length                      │
//! │  Payload  [N]   = H.264 Annex-B data chunk            │
//! └──────────────────────────────────────────────────────┘
//! Total header = 27 bytes.  Max payload per packet = 1373 bytes (fits 1400 MTU).

use anyhow::{anyhow, Result};

pub const MAGIC: u32 = 0x4C414E53; // "LANS"
pub const VERSION: u8 = 1;
pub const HEADER_SIZE: usize = 27;
pub const MAX_PAYLOAD: usize = 1373;
pub const FLAG_KEYFRAME: u8 = 0x01;
pub const FLAG_FRAG_END: u8 = 0x02;
pub const FLAG_PARITY: u8 = 0x04; // FEC parity packet marker

// ── RTCP-lite ──────────────────────────────────────────────────────────────
/// Distinct magic so receivers can tell RTCP from RTP at a glance.
pub const RTCP_MAGIC: u32 = 0x4C524350; // "LRCP"
pub const RTCP_SIZE: usize = 16;
pub const RTCP_TYPE_PROBE: u8 = 1; // host → client (can optionally be 20 bytes with [rtt_ms:4] appended)
pub const RTCP_TYPE_ACK: u8 = 2; // client → host

/// Wire layout: [magic:4][type:1][pad:3][timestamp_us:8]
pub fn build_rtcp(packet_type: u8, timestamp_us: u64) -> [u8; RTCP_SIZE] {
    let mut b = [0u8; RTCP_SIZE];
    b[0..4].copy_from_slice(&RTCP_MAGIC.to_le_bytes());
    b[4] = packet_type;
    b[8..16].copy_from_slice(&timestamp_us.to_le_bytes());
    b
}

/// Returns `(packet_type, timestamp_us)` or `None` if not an RTCP packet.
pub fn parse_rtcp(data: &[u8]) -> Option<(u8, u64)> {
    if data.len() < RTCP_SIZE {
        return None;
    }
    let magic = u32::from_le_bytes(data[0..4].try_into().ok()?);
    if magic != RTCP_MAGIC {
        return None;
    }
    let pkt_type = data[4];
    let ts = u64::from_le_bytes(data[8..16].try_into().ok()?);
    Some((pkt_type, ts))
}

#[derive(Debug, Clone)]
pub struct RtpPacket {
    pub flags: u8,
    pub seq: u16,
    pub timestamp_us: u64,
    pub width: u16,
    pub height: u16,
    pub frag_idx: u16,
    pub frag_total: u16,
    pub payload: Vec<u8>,
    pub display_id: u8,
}

impl RtpPacket {
    pub fn is_keyframe(&self) -> bool {
        self.flags & FLAG_KEYFRAME != 0
    }
    #[allow(dead_code)]
    pub fn is_last_fragment(&self) -> bool {
        self.flags & FLAG_FRAG_END != 0
    }

    /// Serialize to wire bytes
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(HEADER_SIZE + self.payload.len());
        buf.extend_from_slice(&MAGIC.to_le_bytes());
        buf.push(VERSION);
        buf.push(self.flags);
        buf.push(self.display_id);
        buf.extend_from_slice(&self.seq.to_le_bytes());
        buf.extend_from_slice(&self.timestamp_us.to_le_bytes());
        buf.extend_from_slice(&self.width.to_le_bytes());
        buf.extend_from_slice(&self.height.to_le_bytes());
        buf.extend_from_slice(&self.frag_idx.to_le_bytes());
        buf.extend_from_slice(&self.frag_total.to_le_bytes());
        buf.extend_from_slice(&(self.payload.len() as u16).to_le_bytes());
        buf.extend_from_slice(&self.payload);
        buf
    }

    /// Parse from wire bytes
    pub fn from_bytes(data: &[u8]) -> Result<Self> {
        if data.len() < HEADER_SIZE {
            return Err(anyhow!("Packet too short: {} bytes", data.len()));
        }
        let magic = u32::from_le_bytes(data[0..4].try_into()?);
        if magic != MAGIC {
            return Err(anyhow!("Invalid magic: 0x{:08X}", magic));
        }
        let version = data[4];
        if version != VERSION {
            return Err(anyhow!("Unsupported version: {}", version));
        }
        let flags = data[5];
        let display_id = data[6];
        let seq = u16::from_le_bytes(data[7..9].try_into()?);
        let ts = u64::from_le_bytes(data[9..17].try_into()?);
        let width = u16::from_le_bytes(data[17..19].try_into()?);
        let height = u16::from_le_bytes(data[19..21].try_into()?);
        let frag_idx = u16::from_le_bytes(data[21..23].try_into()?);
        let frag_tot = u16::from_le_bytes(data[23..25].try_into()?);
        let data_len = u16::from_le_bytes(data[25..27].try_into()?) as usize;

        if data.len() < HEADER_SIZE + data_len {
            return Err(anyhow!("Truncated payload"));
        }
        Ok(Self {
            flags,
            seq,
            timestamp_us: ts,
            width,
            height,
            frag_idx,
            frag_total: frag_tot,
            payload: data[HEADER_SIZE..HEADER_SIZE + data_len].to_vec(),
            display_id,
        })
    }
}

/// Packetize a single encoded frame into MTU-sized RTP packets.
pub fn packetize(
    nal_data: &[u8],
    seq: &mut u16,
    timestamp_us: u64,
    width: u16,
    height: u16,
    is_keyframe: bool,
    display_id: u8,
) -> Vec<RtpPacket> {
    let chunks: Vec<&[u8]> = nal_data.chunks(MAX_PAYLOAD).collect();
    let total = chunks.len() as u16;
    let mut packets = Vec::with_capacity(chunks.len());

    for (i, chunk) in chunks.iter().enumerate() {
        let mut flags = 0u8;
        if is_keyframe {
            flags |= FLAG_KEYFRAME;
        }
        if i == chunks.len() - 1 {
            flags |= FLAG_FRAG_END;
        }

        packets.push(RtpPacket {
            flags,
            seq: *seq,
            timestamp_us,
            width,
            height,
            frag_idx: i as u16,
            frag_total: total,
            payload: chunk.to_vec(),
            display_id,
        });
        *seq = seq.wrapping_add(1);
    }
    packets
}

/// Reassemble fragmented packets back into a complete NAL unit stream.
///
/// # Memory safety
/// Incomplete frames from dropped packets are evicted after `STALE_TTL_US`
/// to prevent unbounded HashMap growth during lossy sessions.
/// Build a per-frame FEC parity packet (XOR of all fragment payloads).
///
/// Payload layout: [count:1][len0:2][len1:2]…[lenN-1:2][xor_data:max_len]
/// The length prefix lets the receiver recover the exact size of any missing fragment.
pub fn build_parity_packet(
    seq: &mut u16,
    timestamp_us: u64,
    width: u16,
    height: u16,
    frags: &[Vec<u8>],
    frag_total: u16,
    display_id: u8,
) -> RtpPacket {
    let n = frags.len();
    let max_len = frags.iter().map(|p| p.len()).max().unwrap_or(0);

    let mut payload = Vec::with_capacity(1 + n * 2 + max_len);
    payload.push(n as u8);
    for f in frags {
        payload.extend_from_slice(&(f.len() as u16).to_le_bytes());
    }
    let mut xor = vec![0u8; max_len];
    for f in frags {
        for (d, &s) in xor.iter_mut().zip(f.iter()) {
            *d ^= s;
        }
    }
    payload.extend_from_slice(&xor);

    let pkt = RtpPacket {
        flags: FLAG_PARITY | FLAG_FRAG_END,
        seq: *seq,
        timestamp_us,
        width,
        height,
        frag_idx: frag_total, // index = N (one past last data frag)
        frag_total,
        payload,
        display_id,
    };
    *seq = seq.wrapping_add(1);
    pkt
}

pub struct Reassembler {
    frags: std::collections::HashMap<(u64, u8, u16), Vec<u8>>,
    pending_total: std::collections::HashMap<(u64, u8), u16>,
    pending_keyframe: std::collections::HashMap<(u64, u8), bool>,
    arrival_us: std::collections::HashMap<(u64, u8), u64>,
    /// FEC: (lengths_of_each_covered_frag, xor_data)
    parity: std::collections::HashMap<(u64, u8), (Vec<u16>, Vec<u8>)>,
}

/// Fragments older than this are considered permanently lost and evicted.
const STALE_TTL_US: u64 = 2_000_000; // 2 seconds

impl Reassembler {
    pub fn new() -> Self {
        Self {
            frags: std::collections::HashMap::new(),
            pending_total: std::collections::HashMap::new(),
            pending_keyframe: std::collections::HashMap::new(),
            arrival_us: std::collections::HashMap::new(),
            parity: std::collections::HashMap::new(),
        }
    }

    /// Feed a received packet (data or parity).
    /// Returns `Some((timestamp_us, display_id, is_keyframe, nal_data))` when a frame is complete.
    pub fn feed(&mut self, pkt: RtpPacket) -> Option<(u64, u8, bool, Vec<u8>)> {
        let ts = pkt.timestamp_us;
        let display_id = pkt.display_id;
        let key = (ts, display_id);

        // ── FEC parity packet ─────────────────────────────────────────────
        if pkt.flags & FLAG_PARITY != 0 {
            let total = pkt.frag_total;
            self.pending_total.insert(key, total);
            self.arrival_us
                .entry(key)
                .or_insert_with(crate::telemetry::now_us);
            if let Some((lens, xor)) = Self::parse_parity_payload(&pkt.payload) {
                self.parity.insert(key, (lens, xor));
            }
            // Parity arrived — maybe we can now recover exactly one missing frag
            return self.try_recover(ts, display_id);
        }

        // ── Normal data fragment ──────────────────────────────────────────
        self.pending_total.insert(key, pkt.frag_total);
        if pkt.is_keyframe() {
            self.pending_keyframe.insert(key, true);
        }
        self.arrival_us
            .entry(key)
            .or_insert_with(crate::telemetry::now_us);
        self.frags.insert((ts, display_id, pkt.frag_idx), pkt.payload);

        // Evict stale incomplete frames
        let now = crate::telemetry::now_us();
        let stale: Vec<(u64, u8)> = self
            .arrival_us
            .iter()
            .filter(|(_, &arr)| now.saturating_sub(arr) > STALE_TTL_US)
            .map(|(&k, _)| k)
            .collect();
        for s in stale {
            let tot = self.pending_total.remove(&s).unwrap_or(0);
            for i in 0..tot {
                self.frags.remove(&(s.0, s.1, i));
            }
            self.pending_keyframe.remove(&s);
            self.arrival_us.remove(&s);
            self.parity.remove(&s);
            tracing::debug!(ts = s.0, display_id = s.1, "Reassembler: evicted stale frame");
        }

        let total = *self.pending_total.get(&key)?;
        let complete = (0..total).all(|i| self.frags.contains_key(&(ts, display_id, i)));
        if complete {
            return self.assemble(ts, display_id);
        }

        // One frag missing + parity available → try XOR recovery
        if self.parity.contains_key(&key) {
            return self.try_recover(ts, display_id);
        }
        None
    }

    // ── Private helpers ───────────────────────────────────────────────────

    fn parse_parity_payload(data: &[u8]) -> Option<(Vec<u16>, Vec<u8>)> {
        if data.is_empty() {
            return None;
        }
        let n = data[0] as usize;
        if data.len() < 1 + n * 2 {
            return None;
        }
        let mut lens = Vec::with_capacity(n);
        for i in 0..n {
            lens.push(u16::from_le_bytes([data[1 + i * 2], data[2 + i * 2]]));
        }
        Some((lens, data[1 + n * 2..].to_vec()))
    }

    fn try_recover(&mut self, ts: u64, display_id: u8) -> Option<(u64, u8, bool, Vec<u8>)> {
        let key = (ts, display_id);
        let total = *self.pending_total.get(&key)?;
        let (lens, xor) = self.parity.get(&key)?.clone();

        let missing: Vec<u16> = (0..total)
            .filter(|&i| !self.frags.contains_key(&(ts, display_id, i)))
            .collect();
        if missing.len() != 1 {
            return None;
        }
        let m = missing[0] as usize;
        if m >= lens.len() {
            return None;
        }

        // XOR all present frags to get the missing one
        let mut recovered = xor.clone();
        for i in 0..total {
            if i as usize == m {
                continue;
            }
            if let Some(f) = self.frags.get(&(ts, display_id, i)) {
                for (d, &s) in recovered.iter_mut().zip(f.iter()) {
                    *d ^= s;
                }
            }
        }
        recovered.truncate(lens[m] as usize);

        tracing::debug!(
            ts,
            display_id,
            missing_frag = m,
            recovered_len = recovered.len(),
            "FEC: recovered missing fragment via XOR parity"
        );
        self.frags.insert((ts, display_id, missing[0]), recovered);
        self.assemble(ts, display_id)
    }

    fn assemble(&mut self, ts: u64, display_id: u8) -> Option<(u64, u8, bool, Vec<u8>)> {
        let key = (ts, display_id);
        let total = self.pending_total.remove(&key)?;
        let mut nal = Vec::new();
        for i in 0..total {
            if let Some(f) = self.frags.remove(&(ts, display_id, i)) {
                nal.extend_from_slice(&f);
            }
        }
        self.arrival_us.remove(&key);
        self.parity.remove(&key);
        let kf = self.pending_keyframe.remove(&key).unwrap_or(false);
        Some((ts, display_id, kf, nal))
    }
}

// ── Inline stress tests ────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal deterministic LCG — avoids `rand` crate dependency in tests.
    struct Lcg(u64);
    impl Lcg {
        fn new(seed: u64) -> Self {
            Self(seed)
        }
        fn next_f64(&mut self) -> f64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 11) as f64 / (1u64 << 53) as f64
        }
        fn next_usize(&mut self, n: usize) -> usize {
            (self.next_f64() * n as f64) as usize
        }
    }

    fn make_frame(size: usize, seed: u8) -> Vec<u8> {
        (0..size).map(|i| (i as u8).wrapping_add(seed)).collect()
    }

    fn packetize_with_parity(
        nal: &[u8],
        seq: &mut u16,
        ts: u64,
        is_kf: bool,
    ) -> (Vec<RtpPacket>, RtpPacket) {
        let pkts = packetize(nal, seq, ts, 1920, 1080, is_kf, 0);
        let payloads: Vec<Vec<u8>> = pkts.iter().map(|p| p.payload.clone()).collect();
        let total = pkts.len() as u16;
        let parity = build_parity_packet(seq, ts, 1920, 1080, &payloads, total, 0);
        (pkts, parity)
    }

    // ── 1: Zero loss — all frames complete ────────────────────────────────────
    #[test]
    fn test_no_loss_all_frames_complete() {
        let mut r = Reassembler::new();
        let mut seq = 0u16;
        let mut done = 0u32;
        for frame_id in 0u64..200 {
            let ts = frame_id * 16_667;
            let nal = make_frame(2800, frame_id as u8);
            let pkts = packetize(&nal, &mut seq, ts, 1920, 1080, false, 0);
            for p in pkts {
                if r.feed(p).is_some() {
                    done += 1;
                }
            }
        }
        assert_eq!(done, 200, "All 200 zero-loss frames must complete");
    }

    // ── 2: FEC recovers every single-fragment loss ─────────────────────────────
    #[test]
    fn test_fec_single_fragment_recovery() {
        let mut seq = 0u16;
        for frame_id in 0u64..50 {
            let ts = frame_id * 16_667;
            let nal = make_frame(3500, frame_id as u8); // → 3 frags
            let (pkts, parity) = packetize_with_parity(&nal, &mut seq, ts, false);
            if pkts.len() < 2 {
                continue;
            }

            for drop_idx in 0..pkts.len() {
                let mut r = Reassembler::new();
                let mut recovered = false;
                for (i, p) in pkts.iter().enumerate() {
                    if i == drop_idx {
                        continue;
                    }
                    if r.feed(p.clone()).is_some() {
                        recovered = true;
                    }
                }
                if !recovered {
                    if let Some((_, _, _, got)) = r.feed(parity.clone()) {
                        assert_eq!(
                            got, nal,
                            "FEC data mismatch frame={frame_id} drop={drop_idx}"
                        );
                        recovered = true;
                    }
                }
                assert!(
                    recovered,
                    "FEC FAILED frame={frame_id} drop={drop_idx} frags={}",
                    pkts.len()
                );
            }
        }
    }

    // ── 3: 2 missing frags — no false-positive recovery ───────────────────────
    #[test]
    fn test_double_loss_no_false_recovery() {
        let mut seq = 0u16;
        let ts = 99_999u64;
        let nal = make_frame(4200, 42); // → 4 frags
        let (pkts, parity) = packetize_with_parity(&nal, &mut seq, ts, false);
        assert!(pkts.len() >= 3, "Need >= 3 frags for this test");
        let mut r = Reassembler::new();
        for (i, p) in pkts.iter().enumerate() {
            if i == 0 || i == 1 {
                continue;
            } // drop 2 frags
            let _ = r.feed(p.clone());
        }
        assert!(
            r.feed(parity).is_none(),
            "Must NOT recover when 2 fragments are missing"
        );
    }

    // ── 4: Stress — 1000 frames, 5% loss, FEC, GC stability ──────────────────
    #[test]
    fn test_stress_5pct_loss_fec_gc() {
        const FRAMES: u64 = 1_000;
        const LOSS: f64 = 0.05;

        let mut rng = Lcg::new(0xDEAD_BEEF_CAFE_1337);
        let mut r = Reassembler::new();
        let mut seq = 0u16;
        let (mut completed, mut lost, mut fec) = (0u64, 0u64, 0u64);

        for frame_id in 0..FRAMES {
            let ts = frame_id * 16_667;
            let sz = 1024 + rng.next_usize(4096);
            let nal = make_frame(sz, (frame_id & 0xFF) as u8);
            let (pkts, parity) = packetize_with_parity(&nal, &mut seq, ts, frame_id == 0);

            let drops: Vec<bool> = (0..pkts.len()).map(|_| rng.next_f64() < LOSS).collect();
            let n_dropped = drops.iter().filter(|&&d| d).count();

            let mut result = None;
            for (i, p) in pkts.iter().enumerate() {
                if drops[i] {
                    continue;
                }
                if let Some(r) = r.feed(p.clone()) {
                    result = Some(r);
                }
            }
            if result.is_none() {
                if let Some(res) = r.feed(parity) {
                    if n_dropped == 1 {
                        fec += 1;
                    }
                    result = Some(res);
                }
            }

            match result {
                Some((got_ts, got_display, _, got_data)) => {
                    assert_eq!(got_ts, ts, "Timestamp mismatch frame {frame_id}");
                    assert_eq!(got_display, 0, "Display ID mismatch frame {frame_id}");
                    if n_dropped <= 1 {
                        assert_eq!(
                            got_data, nal,
                            "Data corruption frame {frame_id} (drops={n_dropped})"
                        );
                    }
                    completed += 1;
                }
                None => {
                    assert!(
                        n_dropped >= 2,
                        "Frame {frame_id} NOT completed with only {n_dropped} drops!"
                    );
                    lost += 1;
                }
            }
        }

        assert_eq!(
            completed + lost,
            FRAMES,
            "completed + lost must equal FRAMES"
        );
        assert!(
            fec > 0,
            "Expected >= 1 FEC recovery in {FRAMES} frames — got 0"
        );

        println!(
            "Stress: {FRAMES} frames | ok={completed} lost={lost} fec={fec} | \
                  actual_loss={:.1}%",
            lost as f64 / FRAMES as f64 * 100.0
        );
    }

    // ── 5: Out-of-order delivery ───────────────────────────────────────────────
    #[test]
    fn test_out_of_order_fragments() {
        let mut r = Reassembler::new();
        let mut seq = 0u16;
        let mut rng = Lcg::new(0xCAFE_BABE_1234_5678);
        let ts = 1_000_000u64;
        let nal = make_frame(3000, 77);
        let (mut pkts, _) = packetize_with_parity(&nal, &mut seq, ts, true);

        for i in (1..pkts.len()).rev() {
            let j = rng.next_usize(i + 1);
            pkts.swap(i, j);
        }

        let mut result = None;
        for p in pkts {
            if let Some(v) = r.feed(p) {
                result = Some(v);
            }
        }

        let (_, got_display, is_kf, got) = result.expect("Out-of-order frame must complete");
        assert_eq!(got_display, 0, "Display ID mismatch");
        assert!(is_kf, "Keyframe flag must survive reordering");
        assert_eq!(got, nal, "Reordered data must be intact");
    }
}
