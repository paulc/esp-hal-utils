#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

use esp_hal::timer::timg::TimerGroup;
use esp_hal::usb_serial_jtag::UsbSerialJtag;

#[cfg(target_arch = "riscv32")]
use esp_hal::interrupt::software::SoftwareInterruptControl;

use esp_radio::esp_now::{EspNow, PeerInfo, ReceivedData, BROADCAST_ADDRESS};

use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_futures::select::{select3, Either3};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::channel::{Channel, Receiver, Sender};
use embassy_time::{Duration, Ticker};

use esp_now_protocol::{
    BootConfig, BroadcastData, Msg, RxData, Status, TxData, MAX_DATA_LEN, VERSION,
};

use defmt_rtt as _;
use esp_backtrace as _;
use portable_atomic::{AtomicU32, Ordering};
use static_cell::StaticCell;

// use core::fmt::Write;

use esp_hal_utils::format_mac::format_mac;
use esp_hal_utils::serial::{frame_reader, frame_writer, MAX_PAYLOAD_LEN};

extern crate alloc;

// This creates a default app-descriptor required by the esp-idf bootloader.
esp_bootloader_esp_idf::esp_app_desc!();

// TX/RX Channels for serial
static SERIAL_TX: StaticCell<Channel<NoopRawMutex, heapless::Vec<u8, MAX_PAYLOAD_LEN>, 1>> =
    StaticCell::new();
static SERIAL_TX_SENDER: StaticCell<Sender<NoopRawMutex, heapless::Vec<u8, MAX_PAYLOAD_LEN>, 1>> =
    StaticCell::new();
static SERIAL_TX_RECEIVER: StaticCell<
    Receiver<NoopRawMutex, heapless::Vec<u8, MAX_PAYLOAD_LEN>, 1>,
> = StaticCell::new();

static SERIAL_RX: StaticCell<Channel<NoopRawMutex, heapless::Vec<u8, MAX_PAYLOAD_LEN>, 1>> =
    StaticCell::new();
static SERIAL_RX_SENDER: StaticCell<Sender<NoopRawMutex, heapless::Vec<u8, MAX_PAYLOAD_LEN>, 1>> =
    StaticCell::new();
static SERIAL_RX_RECEIVER: StaticCell<
    Receiver<NoopRawMutex, heapless::Vec<u8, MAX_PAYLOAD_LEN>, 1>,
> = StaticCell::new();

// MSG_ID
static MSG_ID: AtomicU32 = AtomicU32::new(0);

const CHANNEL: u8 = 11;

#[derive(PartialEq, Eq, Debug, defmt::Format)]
pub enum AppError {
    EncodingError,
    CapacityError,
    EspNowError,
}

impl core::fmt::Display for AppError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::EncodingError => write!(f, "encoding failed"),
            Self::CapacityError => write!(f, "buffer capacity exceeded"),
            Self::EspNowError => write!(f, "ESP-NOW error"),
        }
    }
}

impl core::error::Error for AppError {}

fn serial_channel_init<'a>() -> (
    &'a Sender<'a, NoopRawMutex, heapless::Vec<u8, MAX_PAYLOAD_LEN>, 1>, // serial_tx
    &'a Receiver<'a, NoopRawMutex, heapless::Vec<u8, MAX_PAYLOAD_LEN>, 1>,
    &'a Sender<'a, NoopRawMutex, heapless::Vec<u8, MAX_PAYLOAD_LEN>, 1>, // serial_rx
    &'a Receiver<'a, NoopRawMutex, heapless::Vec<u8, MAX_PAYLOAD_LEN>, 1>,
) {
    let serial_tx = &*SERIAL_TX.init(Channel::new());
    let serial_tx_sender = &*SERIAL_TX_SENDER.init(serial_tx.sender());
    let serial_tx_receiver = &*SERIAL_TX_RECEIVER.init(serial_tx.receiver());
    let serial_rx = &*SERIAL_RX.init(Channel::new());
    let serial_rx_sender = &*SERIAL_RX_SENDER.init(serial_rx.sender());
    let serial_rx_receiver = &*SERIAL_RX_RECEIVER.init(serial_rx.receiver());
    (
        serial_tx_sender,
        serial_tx_receiver,
        serial_rx_sender,
        serial_rx_receiver,
    )
}

fn encode_msg(msg: &Msg) -> Result<heapless::Vec<u8, MAX_PAYLOAD_LEN>, AppError> {
    let src = msg.to_heapless().map_err(|_| AppError::EncodingError)?;
    heapless::Vec::from_slice(&src).map_err(|_| AppError::CapacityError)
}

fn next_id() -> u32 {
    MSG_ID.fetch_add(1, Ordering::Relaxed)
}

async fn esp_now_init(
    esp_now: &mut EspNow<'_>,
    sender: &Sender<'_, NoopRawMutex, heapless::Vec<u8, MAX_PAYLOAD_LEN>, 1>,
) -> Result<(), AppError> {
    esp_now
        .set_channel(CHANNEL)
        .map_err(|_| AppError::EspNowError)?;
    let msg = Msg::Init(BootConfig {
        id: next_id(),
        api_version: VERSION,
        now_version: esp_now.version().map_err(|_| AppError::EspNowError)?,
        channel: CHANNEL,
        address: esp_radio::wifi::sta_mac(),
    });
    let (_, status) = join(
        sender.send(encode_msg(&msg)?),
        esp_now.send_async(&BROADCAST_ADDRESS, b">> ESP_NOW INIT <<"),
    )
    .await;
    defmt::info!("ESP_NOW INIT: {:?} -> {:?}", msg, status);
    Ok(())
}

async fn handle_send(
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
    defmt::info!(">> Msg::Send: {:?} -> {}", data, status);
    let msg = Msg::Response(Status {
        id: data.id,
        status: status.is_ok(),
    });
    sender.send(encode_msg(&msg)?).await;
    Ok(())
}

async fn handle_broadcast(
    esp_now: &mut EspNow<'_>,
    sender: &Sender<'_, NoopRawMutex, heapless::Vec<u8, MAX_PAYLOAD_LEN>, 1>,
    data: &BroadcastData,
) -> Result<(), AppError> {
    let status = esp_now.send_async(&BROADCAST_ADDRESS, &data.data).await;
    defmt::info!(">> Msg::Broadcast: {:?} -> {}", data, status);
    let msg = Msg::Response(Status {
        id: data.id,
        status: status.is_ok(),
    });
    sender.send(encode_msg(&msg)?).await;
    Ok(())
}

async fn handle_recv(
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
    defmt::info!(">> Msg::Recv: {:?}", msg);
    sender.send(encode_msg(&msg)?).await;
    Ok(())
}

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    let peripherals = esp_hal::init(esp_hal::Config::default());
    esp_alloc::heap_allocator!(size: 64 * 1024);

    defmt::info!("Init!");

    #[cfg(target_arch = "riscv32")]
    let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(
        timg0.timer0,
        #[cfg(target_arch = "riscv32")]
        sw_int.software_interrupt0,
    );

    // Start serial
    let (rx, tx) = UsbSerialJtag::new(peripherals.USB_DEVICE)
        .into_async()
        .split();

    let (serial_tx_sender, serial_tx_receiver, serial_rx_sender, serial_rx_receiver) =
        serial_channel_init();

    spawner.must_spawn(frame_reader(rx, &serial_rx_sender));
    spawner.must_spawn(frame_writer(tx, &serial_tx_receiver));

    // Start ESP-NOW
    defmt::info!("Start ESP_NOW");

    let esp_radio_ctrl = esp_radio::init().unwrap();
    let wifi = peripherals.WIFI;
    let (mut controller, interfaces) =
        esp_radio::wifi::new(&esp_radio_ctrl, wifi, Default::default()).unwrap();
    controller.set_mode(esp_radio::wifi::WifiMode::Sta).unwrap();
    controller.start().unwrap();

    let mut esp_now = interfaces.esp_now;
    esp_now_init(&mut esp_now, &serial_tx_sender).await.unwrap();

    // Main Loop
    let mut ticker = Ticker::every(Duration::from_secs(1));
    let mut rx_count: usize = 0;
    let mut broadcast: Option<(u32, heapless::Vec<u8, 250>)> = None;
    let mut broadcast_counter: u32 = 0;

    loop {
        match select3(
            ticker.next(),
            serial_rx_receiver.receive(),
            esp_now.receive_async(),
        )
        .await
        {
            Either3::First(_) => {
                // Ticker - check broadcast interval
                if let Some((ref interval, ref data)) = broadcast {
                    if broadcast_counter > 0 && broadcast_counter.is_multiple_of(*interval) {
                        // Send broadcast
                        let status = esp_now.send_async(&BROADCAST_ADDRESS, &data).await;
                        defmt::info!(">> BROADCAST -> {:?}", status);
                        broadcast_counter = 0;
                    } else {
                        broadcast_counter += 1;
                    }
                }
            }
            Either3::Second(serial_frame) => {
                // Received serial frame
                rx_count += 1;
                /*
                defmt::info!(
                    ">> SERIAL RX: [{}] {} bytes - {:?}",
                    rx_count,
                    serial_frame.len(),
                    serial_frame[..serial_frame.len().min(8)]
                );
                */
                // Try to decode message
                match Msg::from_slice(&serial_frame) {
                    Ok(msg) => match msg {
                        Msg::Send(data) => {
                            match handle_send(&mut esp_now, &serial_tx_sender, &data).await {
                                Ok(_) => defmt::info!(">>> Msg::Send OK"),
                                Err(e) => defmt::error!(">>> Msg::Send Error: {}", e),
                            }
                        }
                        Msg::Broadcast(data) => {
                            match handle_broadcast(&mut esp_now, &serial_tx_sender, &data).await {
                                Ok(_) => defmt::info!(">>> Msg::Broadcast OK"),
                                Err(e) => defmt::error!(">>> Msg::Broadcast Error: {}", e),
                            }
                            // Set broadcast interval timer
                            if let Some(interval) = data.interval {
                                broadcast = Some((interval, data.data));
                                broadcast_counter = 0; // Reset counter
                            }
                        }
                        _ => defmt::info!(">>> Msg [unknown]"),
                    },
                    Err(_) => defmt::error!(">>> Invalid Msg"),
                }
            }
            Either3::Third(rx_data) => {
                /*
                defmt::info!(
                    ">> ESP-NOW RX: [{}]->[{}] >> {} [rssi={}]",
                    format_mac(&rx_data.info.src_address),
                    format_mac(&rx_data.info.dst_address),
                    match core::str::from_utf8(rx_data.data()) {
                        Ok(s) => s,
                        Err(_) => "<<DATA>>",
                    },
                    rx_data.info.rx_control.rssi
                );
                */
                match handle_recv(&serial_tx_sender, &rx_data).await {
                    Ok(_) => defmt::info!(">>> Msg::Recv OK"),
                    Err(e) => defmt::error!("Msg::Recv Error: {}", e),
                }
            }
        }
    }
}
