use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::channel::{Channel, Receiver, Sender};

use static_cell::StaticCell;

use esp_hal_utils::serial::MAX_PAYLOAD_LEN;

// TX/RX Channels for serial task
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

pub fn init<'a>() -> (
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
