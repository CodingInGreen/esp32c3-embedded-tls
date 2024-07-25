#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]

use core::str;
use embassy_executor::Spawner;
use embassy_net::{tcp::TcpSocket, Config, Ipv4Address, Stack, StackResources};
use embassy_time::{Duration, Timer as EmbassyTimer};
use embedded_tls::{Aes128GcmSha256, TlsConfig, TlsConnection, TlsContext};
use esp_hal::entry;
use esp_hal::peripherals::TIMG0;
use esp_hal::prelude::_esp_hal_timer_Timer;
use esp_hal::prelude::main;
use esp_hal::timer::timg::Timer0;
use esp_hal::timer::timg::TimerX;
use esp_hal::{
    clock::ClockControl,
    peripherals::Peripherals,
    rng::Rng,
    system::SystemControl,
    timer::{
        timg::{Timer, TimerGroup},
        OneShotTimer, PeriodicTimer,
    },
};
use esp_hal_embassy;
use esp_println::println;
use esp_wifi::wifi::WifiDevice;
use esp_wifi::EspWifiInitFor;
use esp_wifi::{
    initialize,
    wifi::{ClientConfiguration, Configuration, WifiController, WifiStaDevice},
};
use fugit;
use esp_hal::prelude::_fugit_ExtU64;
use heapless::String;
use panic_halt as _;
use static_cell::StaticCell;

// WiFi
const SSID: &str = env!("SSID");
const PASSWORD: &str = env!("PASSWORD");

const CONNECT_ATTEMPTS: usize = 10;
const RETRY_DELAY_MS: u64 = 5000;

#[main]
async fn main(spawner: Spawner) {
    esp_println::logger::init_logger_from_env();

    println!("Starting program...");

    //spawner.spawn(print_int(41)).unwrap();

    let peripherals = Peripherals::take();
    let system = SystemControl::new(peripherals.SYSTEM);
    let clocks = ClockControl::max(system.clock_control).freeze();

    let mut timer_group = TimerGroup::new(peripherals.TIMG0, &clocks, None);
    let mut timer0 = timer_group.timer0;

    let timer = esp_hal::timer::systimer::SystemTimer::new(peripherals.SYSTIMER).alarm0;

    // Start the timer
    timer0.start();

    let init = match initialize(
        EspWifiInitFor::Wifi,
        timer,
        Rng::new(peripherals.RNG),
        peripherals.RADIO_CLK,
        &clocks,
    ) {
        Ok(init) => {
            println!("Wi-Fi initialization successful.");
            init
        }
        Err(e) => {
            println!("Wi-Fi initialization failed: {:?}", e);
            return;
        }
    };

    let wifi = peripherals.WIFI;
    let (wifi_interface, mut controller) =
        esp_wifi::wifi::new_with_mode(&init, wifi, WifiStaDevice).unwrap();

    let mut ssid: String<32> = String::new();
    let mut password: String<64> = String::new();
    ssid.push_str(SSID).unwrap();
    password.push_str(PASSWORD).unwrap();

    let client_config = ClientConfiguration {
        ssid,
        password,
        ..Default::default()
    };

    controller
        .set_configuration(&Configuration::Client(client_config))
        .unwrap();
    controller.start().await.unwrap();
    println!("WiFi Started...");


    let mut attempts = 0;
    loop {
        attempts += 1;
        println!("Attempt {}: Connecting to Wi-Fi...", attempts);

        if let Ok(()) = controller.connect().await {

        // After starting Wi-Fi and setting configuration
        if let Ok(is_connected) = controller.is_connected() {
            if is_connected {
                println!("Wi-Fi connected successfully.");
            } else {
                println!("Wi-Fi is not connected.");
            }
        } else {
            println!("Error checking Wi-Fi connection status.");
        }
            break;
        }

        if attempts >= CONNECT_ATTEMPTS {
            println!("Failed to connect to Wi-Fi after {} attempts.", CONNECT_ATTEMPTS);
            return;
        }

        println!("Retrying in {} ms...", RETRY_DELAY_MS);
        EmbassyTimer::after(Duration::from_millis(RETRY_DELAY_MS)).await;
    }

    let config = Config::dhcpv4(Default::default());
    let seed = 1234;

    static STACK: StaticCell<Stack<WifiDevice<'_, WifiStaDevice>>> = StaticCell::new();
    static RESOURCES: StaticCell<StackResources<3>> = StaticCell::new();
    let stack = &*STACK.init(Stack::new(
        wifi_interface,
        config,
        RESOURCES.init(StackResources::<3>::new()),
        seed,
    ));

    // Launch network task that runs `stack.run().await`
    spawner.spawn(net_task(stack)).unwrap();
    // Wait for DHCP config
    stack.wait_config_up().await;

    // Check the stack configuration
    let config_v4 = stack.config_v4();
    if let Some(config) = config_v4 {
        println!("IP Address: {:?}", config.address);
    } else {
        println!("Failed to obtain IP address.");
    }

    println!("Stack IP Configuration: {:?}", stack.config_v4());

    let mut rx_buffer = [0; 4096];
    let mut tx_buffer = [0; 4096];

    println!("Connected to Wi-Fi, starting main loop...");
    let mut socket = TcpSocket::new(stack, &mut rx_buffer, &mut tx_buffer);

    if let Err(e) = socket.connect((Ipv4Address::new(142, 250, 185, 115), 80)).await {
        println!("Failed to open socket: {:?}", e);
    }

    if let Err(e) = socket.write(b"GET / HTTP/1.0\r\nHost: www.mobile-j.de\r\n\r\n").await {
        println!("Failed to write to socket: {:?}", e);
    }

    if let Err(e) = socket.flush().await {
        println!("Failed to flush socket: {:?}", e);
    }


    let mut response = [0; 512];
        if let Ok(size) = socket.read(&mut response).await {
            if let Ok(text) = core::str::from_utf8(&response[..size]) {
                println!("{}", text);
            }
        }

    socket.close();
    /* 
    loop {
        println!("Making HTTP request");

        if let Err(e) = socket.connect((Ipv4Address::new(142, 250, 185, 115), 80)).await {
            println!("Failed to open socket: {:?}", e);
            continue;
        }

        if let Err(e) = socket.write(b"GET / HTTP/1.0\r\nHost: www.mobile-j.de\r\n\r\n").await {
            println!("Failed to write to socket: {:?}", e);
            continue;
        }

        if let Err(e) = socket.flush().await {
            println!("Failed to flush socket: {:?}", e);
        }

        let mut response = [0; 512];
        if let Ok(size) = socket.read(&mut response).await {
            if let Ok(text) = core::str::from_utf8(&response[..size]) {
                println!("{}", text);
            }
        }

        socket.close();

        println!("Delaying for 5 seconds before next request...");
        EmbassyTimer::after(Duration::from_secs(5)).await;
    }
    */
}

/*
let init = initialize(
    EspWifiInitFor::Wifi,
    timer,
    Rng::new(peripherals.RNG),
    peripherals.RADIO_CLK,
    &clocks,
)
.unwrap();

let wifi = peripherals.WIFI;
let (wifi_interface, controller) =
    esp_wifi::wifi::new_with_mode(&init, wifi, WifiStaDevice).unwrap();

let config = Config::dhcpv4(Default::default());

let seed = 1234;

static STACK: StaticCell<Stack<WifiDevice<'_, WifiStaDevice>>> = StaticCell::new();
static RESOURCES: StaticCell<StackResources<3>> = StaticCell::new();
let stack = &*STACK.init(Stack::new(
    wifi_interface,
    config,
    RESOURCES.init(StackResources::<3>::new()),
    seed,
));

let mut rx_buffer = [0; 4096];
let mut tx_buffer = [0; 4096];

println!("Waiting to get IP address...");
while !stack.is_link_up() {
    EmbassyTimer::after(Duration::from_millis(500)).await;
}

println!("Got IP: {:?}", stack.config_v4().unwrap().address);

*/

/*
    let timer_group = TimerGroup::new(peripherals.TIMG0, &clocks, None);
    let timer = timer_group.timer0;

    let timer = esp_hal::timer::systimer::SystemTimer::new(peripherals.SYSTIMER).alarm0;

     let init = initialize(
         EspWifiInitFor::Wifi,
         timer,
         Rng::new(peripherals.RNG),
         peripherals.RADIO_CLK,
         &clocks,
     )
     .unwrap();

     let wifi = peripherals.WIFI;
     let (wifi_interface, controller) =
         esp_wifi::wifi::new_with_mode(&init, wifi, WifiStaDevice).unwrap();

     let config = Config::dhcpv4(Default::default());

     let seed = 1234;

     static STACK: StaticCell<Stack<WifiStaDevice>> = StaticCell::new();
     static RESOURCES: StaticCell<StackResources<3>> = StaticCell::new();
     let stack = &*STACK.init(Stack::new(
         wifi_interface,
         config,
         RESOURCES.init(StackResources::<3>::new()),
         seed,
     ));


     let mut rx_buffer = [0; 4096];
     let mut tx_buffer = [0; 4096];

     println!("Waiting to get IP address...");
     while !stack.is_link_up() {
         Timer::after(Duration::from_millis(500)).await;
     }

     println!("Got IP: {:?}", stack.config_v4().unwrap().address);

    // let remote_endpoint = (Ipv4Address::new(142, 250, 185, 115), 443); // Google IP for testing
    // let mut socket = TcpSocket::new(stack, &mut rx_buffer, &mut tx_buffer);
    // socket.set_timeout(Some(Duration::from_secs(10)));

    // println!("Connecting to {:?}...", remote_endpoint);
    // socket.connect(remote_endpoint).await.unwrap();
    // println!("Connected!");

    // let mut read_record_buffer = [0; 16384];
    // let mut write_record_buffer = [0; 16384];
    // let config = TlsConfig::new().with_server_name("www.google.com");
    // let mut tls = TlsConnection::new(socket, &mut read_record_buffer, &mut write_record_buffer);

    // tls.open(TlsContext::new(
    //     &config,
    //     UnsecureProvider::new::<Aes128GcmSha256>(rand::rngs::OsRng),
    // ))
    // .await
    // .unwrap();

    // tls.write_all(b"GET / HTTP/1.1\r\nHost: www.google.com\r\n\r\n").await.unwrap();
    // tls.flush().await.unwrap();

    // let mut response = [0; 1024];
    // let size = tls.read(&mut response).await.unwrap();
    // println!("{}", str::from_utf8(&response[..size]).unwrap());
}

// #[embassy_executor::task]
// async fn connection(mut controller: WifiController<'static>) {
//     loop {
//         if controller.is_started().unwrap_or(false) && esp_wifi::wifi::get_wifi_state() == esp_wifi::wifi::WifiState::StaConnected {
//             Timer::after(Duration::from_millis(5000)).await;
//         } else {
//             let client_config = ClientConfiguration {
//                 ssid: env!("SSID").into(),
//                 password: env!("PASSWORD").into(),
//                 ..Default::default()
//             };
//             controller.set_configuration(&Configuration::Client(client_config)).unwrap();
//             controller.start().await.unwrap();
//             controller.connect().await.unwrap();
//             println!("Connected to WiFi");
//         }
//     }
// }

// #[embassy_executor::task]
// async fn net_task(stack: &'static Stack<WifiStaDevice>) {
//     stack.run().await;
// }

*/

/*

#[embassy_executor::task]
async fn print_int(variable: i32 ) {
     println!("Integer: {}", variable);
 }

 */

 #[embassy_executor::task]
async fn net_task(stack: &'static Stack<WifiDevice<'static, WifiStaDevice>>) {
    stack.run().await
}