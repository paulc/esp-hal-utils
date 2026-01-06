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

// use esp_radio::esp_now::{EspNow, EspNowWifiInterface, PeerInfo, BROADCAST_ADDRESS};
use esp_radio::esp_now::BROADCAST_ADDRESS;

use embassy_executor::Spawner;
use embassy_futures::select::{select3, Either3};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::channel::{Channel, Receiver, Sender};
use embassy_time::{Duration, Ticker};

use defmt_rtt as _;
use esp_backtrace as _;
use static_cell::StaticCell;

use core::fmt::Write;

use esp_hal_utils::format_mac::format_mac;
use esp_hal_utils::serial::{frame_reader, frame_writer, MAX_PAYLOAD_LEN};

extern crate alloc;

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

// Static ESP-NOW instance
// static RADIO_CTRL: StaticCell<esp_radio::Controller<'static>> = StaticCell::new();
// static ESP_NOW: StaticCell<EspNow<'static>> = StaticCell::new();

// TX/RX Channels for serial
static CHANNEL_TX: StaticCell<Channel<NoopRawMutex, heapless::Vec<u8, MAX_PAYLOAD_LEN>, 1>> =
    StaticCell::new();
static CHANNEL_TX_SENDER: StaticCell<Sender<NoopRawMutex, heapless::Vec<u8, MAX_PAYLOAD_LEN>, 1>> =
    StaticCell::new();
static CHANNEL_TX_RECEIVER: StaticCell<
    Receiver<NoopRawMutex, heapless::Vec<u8, MAX_PAYLOAD_LEN>, 1>,
> = StaticCell::new();

static CHANNEL_RX: StaticCell<Channel<NoopRawMutex, heapless::Vec<u8, MAX_PAYLOAD_LEN>, 1>> =
    StaticCell::new();
static CHANNEL_RX_SENDER: StaticCell<Sender<NoopRawMutex, heapless::Vec<u8, MAX_PAYLOAD_LEN>, 1>> =
    StaticCell::new();
static CHANNEL_RX_RECEIVER: StaticCell<
    Receiver<NoopRawMutex, heapless::Vec<u8, MAX_PAYLOAD_LEN>, 1>,
> = StaticCell::new();

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    let peripherals = esp_hal::init(esp_hal::Config::default());
    let rtc = esp_hal::rtc_cntl::Rtc::new(peripherals.LPWR);

    let boot_time = rtc.time_since_boot().as_millis();
    let wakeup_cause = esp_hal::rtc_cntl::wakeup_cause();

    defmt::info!("INIT: boot_time={} wakeup={}", boot_time, wakeup_cause);

    esp_alloc::heap_allocator!(size: 64 * 1024);

    #[cfg(target_arch = "riscv32")]
    let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(
        timg0.timer0,
        #[cfg(target_arch = "riscv32")]
        sw_int.software_interrupt0,
    );

    // Start serial tasks
    defmt::info!("Start UsbSerial");
    let (rx, tx) = UsbSerialJtag::new(peripherals.USB_DEVICE)
        .into_async()
        .split();

    let channel_tx = &*CHANNEL_TX.init(Channel::new());
    let channel_tx_sender = &*CHANNEL_TX_SENDER.init(channel_tx.sender());
    let channel_tx_receiver = &*CHANNEL_TX_RECEIVER.init(channel_tx.receiver());
    let channel_rx = &*CHANNEL_RX.init(Channel::new());
    let channel_rx_sender = &*CHANNEL_RX_SENDER.init(channel_rx.sender());
    let channel_rx_receiver = &*CHANNEL_RX_RECEIVER.init(channel_rx.receiver());

    spawner.spawn(frame_reader(rx, &channel_rx_sender)).unwrap();
    spawner
        .spawn(frame_writer(tx, &channel_tx_receiver))
        .unwrap();

    // Initialise ESP-NOW
    defmt::info!("Start ESP_NOW");
    let esp_radio_ctrl = esp_radio::init().unwrap();

    let wifi = peripherals.WIFI;
    let (mut controller, interfaces) =
        esp_radio::wifi::new(&esp_radio_ctrl, wifi, Default::default()).unwrap();
    controller.set_mode(esp_radio::wifi::WifiMode::Sta).unwrap();
    controller.start().unwrap();

    let mut esp_now = interfaces.esp_now;

    esp_now.set_channel(11).unwrap();
    defmt::info!(
        "ESP-NOW: mac={} version={} peers={}",
        format_mac(&esp_radio::wifi::sta_mac()),
        esp_now.version().unwrap(),
        esp_now.peer_count().unwrap()
    );

    // Try send
    let status = esp_now
        .send_async(&BROADCAST_ADDRESS, b">> ESP-NOW INIT <<")
        .await;
    defmt::info!("ESP-NOW: Send start -> {:?}", status);

    let mut ticker = Ticker::every(Duration::from_secs(1));
    let mut rx_count = 0_usize;
    let mut msg = heapless::String::<64>::new();

    defmt::info!("Start Main Loop");
    loop {
        match select3(
            ticker.next(),
            channel_rx_receiver.receive(),
            esp_now.receive_async(),
        )
        .await
        {
            Either3::First(_) => {
                msg.clear();
                let now = rtc.time_since_boot().as_millis();
                write!(
                    &mut msg,
                    "STATUS -> [{}] Serial RX: {}",
                    now / 1000,
                    rx_count
                )
                .unwrap();
                let data: heapless::Vec<u8, MAX_PAYLOAD_LEN> = msg.as_bytes().try_into().unwrap();
                channel_tx_sender.send(data).await;
                let status = esp_now.send_async(&BROADCAST_ADDRESS, msg.as_bytes()).await;
                defmt::info!(">>> Status: {} esp_now={:?}", msg, status);
            }
            Either3::Second(frame) => {
                rx_count += 1;
                defmt::info!(
                    ">>> RX Frame: [{}] {} bytes - {:?}",
                    rx_count,
                    frame.len(),
                    frame[..frame.len().min(8)]
                );
            }
            Either3::Third(r) => {
                defmt::info!("ESP-NOW: Received {:?}", r);
            }
        }
    }
}

/*
#[embassy_executor::task]
async fn esp_now_task(esp_now: &'static mut EspNow<'static>) {
    esp_now.set_channel(11).unwrap();
    defmt::info!(
        "ESP-NOW: mac={} version={} peers={}",
        format_mac(&esp_radio::wifi::sta_mac()),
        esp_now.version().unwrap(),
        esp_now.peer_count().unwrap()
    );
    defmt::info!(">> TRY SEND");
    let sw = esp_now
        .send(&BROADCAST_ADDRESS, b"BLOCKING")
        .expect("ESP-NOW send");
    let status = sw.wait();
    defmt::info!("ESP-NOW: Send broadcast -> {:?}", status);
    let mut ticker = Ticker::every(Duration::from_secs(5));
    loop {
        ticker.next().await;
        defmt::info!(">> NOW");
        let status = esp_now
            .send_async(&BROADCAST_ADDRESS, b"=== BROADCAST ===")
            .await;
        defmt::info!("ESP-NOW: Send broadcast -> {:?}", status);
        defmt::info!("ESP-NOW: peers={}", esp_now.peer_count().unwrap());
        match select(ticker.next(), async {
            // Wait for ESP-NOW RX
            let r = esp_now.receive_async().await;
            defmt::info!("ESP-NOW: Received {:?}", r);
            if r.info.dst_address == BROADCAST_ADDRESS {
                if !esp_now.peer_exists(&r.info.src_address) {
                    esp_now
                        .add_peer(PeerInfo {
                            interface: EspNowWifiInterface::Sta,
                            peer_address: r.info.src_address,
                            lmk: None,
                            channel: None,
                            encrypt: false,
                        })
                        .unwrap();
                }
                // let status = esp_now.send_async(&r.info.src_address, b"Hello Peer").await;
                // defmt::info!("ESP-NOW: send: {} -> {}", r.info.src_address, status);
            }
        })
        .await
        {
            Either::First(_) => {
                defmt::info!("ESP-NOW >> RX TIMEOUT");
                // let status = esp_now.send_async(&BROADCAST_ADDRESS, b"BROADCAST").await;
                // defmt::info!("ESP-NOW: Send broadcast -> {:?}", status);
                // defmt::info!("ESP-NOW: peers={}", esp_now.peer_count().unwrap());
            }
            Either::Second(_) => (),
        }
    }
}
*/
