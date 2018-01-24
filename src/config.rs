use picoquic_sys::picoquic::PICOQUIC_RESET_SECRET_SIZE;

pub struct Config {
    pub cert_filename: String,
    pub key_filename: String,
    pub reset_seed: Option<[u8; PICOQUIC_RESET_SECRET_SIZE as usize]>,
}
