use esp_radio::esp_now::{EspNow, PeerInfo, ReceivedData, BROADCAST_ADDRESS};

use embassy_futures::join::join;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::channel::Sender;

use esp_now_protocol::{
    BroadcastData, InitConfig, Msg, RxData, Status, TxData, MAX_DATA_LEN, VERSION,
};

use portable_atomic::{AtomicU32, Ordering};

use crate::error::AppError;
use esp_hal_utils::serial::MAX_PAYLOAD_LEN;

const DEFAULT_CHANNEL: u8 = 11;

static MSG_ID: AtomicU32 = AtomicU32::new(0);

fn encode_msg(msg: &Msg) -> Result<heapless::Vec<u8, MAX_PAYLOAD_LEN>, AppError> {
    msg.to_heapless::<MAX_PAYLOAD_LEN>()
        .map_err(|_| AppError::EncodingError)
}

fn next_id() -> u32 {
    MSG_ID.fetch_add(1, Ordering::Relaxed)
}

pub async fn handle_init(
    esp_now: &mut EspNow<'_>,
    sender: &Sender<'_, NoopRawMutex, heapless::Vec<u8, MAX_PAYLOAD_LEN>, 1>,
) -> Result<(), AppError> {
    esp_now
        .set_channel(DEFAULT_CHANNEL)
        .map_err(|_| AppError::EspNowError)?;
    let msg = Msg::Init(InitConfig {
        id: next_id(),
        api_version: VERSION,
        now_version: esp_now.version().map_err(|_| AppError::EspNowError)?,
        channel: DEFAULT_CHANNEL,
        address: esp_radio::wifi::sta_mac(),
    });
    let c = InitConfig {
        id: next_id(),
        api_version: VERSION,
        now_version: esp_now.version().map_err(|_| AppError::EspNowError)?,
        channel: DEFAULT_CHANNEL,
        address: esp_radio::wifi::sta_mac(),
    };
    defmt::info!("+++++ {}", c);
    let (_, status) = join(
        sender.send(encode_msg(&msg)?),
        esp_now.send_async(&BROADCAST_ADDRESS, b">> ESP_NOW INIT <<"),
    )
    .await;
    defmt::info!("ESP_NOW INIT: {} -> {:?}", msg, status);
    Ok(())
}

pub async fn handle_send(
    esp_now: &mut EspNow<'_>,
    sender: &Sender<'_, NoopRawMutex, heapless::Vec<u8, MAX_PAYLOAD_LEN>, 1>,
    data: &TxData,
) -> Result<(), AppError> {
    // Add peer if necessary
    if !esp_now.peer_exists(&data.dst_addr) {
        esp_now
            .add_peer(PeerInfo {
                interface: esp_radio::esp_now::EspNowWifiInterface::Sta,
                peer_address: data.dst_addr,
                lmk: None,
                channel: None,
                encrypt: false,
            })
            .map_err(|_| AppError::EspNowError)?;
    }
    let status = esp_now.send_async(&data.dst_addr, &data.data).await;
    defmt::info!(">> Msg::Send: {} -> {}", data, status);
    let msg = Msg::Response(Status {
        id: data.id,
        status: status.is_ok(),
    });
    sender.send(encode_msg(&msg)?).await;
    Ok(())
}

pub async fn handle_broadcast(
    esp_now: &mut EspNow<'_>,
    sender: &Sender<'_, NoopRawMutex, heapless::Vec<u8, MAX_PAYLOAD_LEN>, 1>,
    data: &BroadcastData,
) -> Result<(), AppError> {
    let status = esp_now.send_async(&BROADCAST_ADDRESS, &data.data).await;
    defmt::info!(">> Msg::Broadcast: {} -> {}", data, status);
    let msg = Msg::Response(Status {
        id: data.id,
        status: status.is_ok(),
    });
    sender.send(encode_msg(&msg)?).await;
    Ok(())
}

pub async fn handle_recv(
    sender: &Sender<'_, NoopRawMutex, heapless::Vec<u8, MAX_PAYLOAD_LEN>, 1>,
    data: &ReceivedData,
) -> Result<(), AppError> {
    let msg = Msg::Recv(RxData {
        id: next_id(),
        src_addr: data.info.src_address,
        dst_addr: data.info.dst_address,
        rssi: data.info.rx_control.rssi,
        data: heapless::Vec::<u8, MAX_DATA_LEN>::from_slice(data.data())
            .map_err(|_| AppError::CapacityError)?,
    });
    defmt::info!(">> Msg::Recv: {}", msg);
    sender.send(encode_msg(&msg)?).await;
    Ok(())
}
