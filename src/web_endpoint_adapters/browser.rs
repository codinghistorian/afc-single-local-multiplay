use core::fmt;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::io;
use std::rc::Rc;

use js_sys::{ArrayBuffer, Promise, Reflect, Uint8Array};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{
    BinaryType, Event, MessageEvent, ReadableStream, ReadableStreamDefaultReader, WebSocket,
    WritableStream, WritableStreamDefaultWriter,
};

use super::{
    AFC_WEBSOCKET_SUBPROTOCOL, WebEndpointConfig, WebEndpointConfigError, WebEndpointKind,
    WebEndpointMetrics,
};
use crate::network_io::{
    AfcDatagram, MAX_AFC_DATAGRAM_BYTES, NonBlockingDatagramEndpoint, ReceiveOutcome, SendOutcome,
};

const BROWSER_ADMISSION_TIMEOUT_MS: i32 = 10_000;

// web-sys still gates WebTransport itself behind its unstable-API cfg even
// though the browser API is shipping. Keep the small stable surface used here
// explicit while using web-sys's stable Streams bindings.
#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_name = WebTransport)]
    #[derive(Clone)]
    type JsWebTransport;

    #[wasm_bindgen(constructor, catch, js_class = WebTransport)]
    fn new(url: &str) -> Result<JsWebTransport, JsValue>;

    #[wasm_bindgen(method, getter)]
    fn ready(this: &JsWebTransport) -> Promise;

    #[wasm_bindgen(method, getter)]
    fn closed(this: &JsWebTransport) -> Promise;

    #[wasm_bindgen(method, getter)]
    fn datagrams(this: &JsWebTransport) -> JsWebTransportDatagrams;

    #[wasm_bindgen(method, js_name = createBidirectionalStream)]
    fn create_bidirectional_stream(this: &JsWebTransport) -> Promise;

    #[wasm_bindgen(method)]
    fn close(this: &JsWebTransport);

    #[derive(Clone)]
    type JsWebTransportBidirectionalStream;

    #[wasm_bindgen(method, getter)]
    fn readable(this: &JsWebTransportBidirectionalStream) -> ReadableStream;

    #[wasm_bindgen(method, getter)]
    fn writable(this: &JsWebTransportBidirectionalStream) -> WritableStream;

    #[derive(Clone)]
    type JsWebTransportDatagrams;

    #[wasm_bindgen(method, getter)]
    fn readable(this: &JsWebTransportDatagrams) -> ReadableStream;

    #[wasm_bindgen(method, getter)]
    fn writable(this: &JsWebTransportDatagrams) -> WritableStream;

    #[wasm_bindgen(catch, method, js_name = createWritable)]
    fn create_writable(this: &JsWebTransportDatagrams) -> Result<WritableStream, JsValue>;

    #[wasm_bindgen(method, getter, js_name = maxDatagramSize)]
    fn max_datagram_size(this: &JsWebTransportDatagrams) -> u32;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BrowserConnectionState {
    Connecting,
    Open,
    Disconnected,
    Failed(io::ErrorKind),
    Oversized(usize),
}

struct BrowserInboundState {
    config: WebEndpointConfig,
    connection: BrowserConnectionState,
    admission_pending: bool,
    admission_timeout_handle: Option<i32>,
    inbound: VecDeque<AfcDatagram>,
    metrics: WebEndpointMetrics,
}

impl BrowserInboundState {
    fn new(config: WebEndpointConfig, admission_pending: bool) -> Self {
        Self {
            config,
            connection: BrowserConnectionState::Connecting,
            admission_pending,
            admission_timeout_handle: None,
            inbound: VecDeque::with_capacity(config.inbound_capacity_packets),
            metrics: WebEndpointMetrics::default(),
        }
    }

    fn receive(&mut self) -> ReceiveOutcome {
        if let Some(datagram) = self.inbound.pop_front() {
            self.metrics.inbound_depth_packets = self.inbound.len();
            return ReceiveOutcome::Received(datagram);
        }
        match self.connection {
            BrowserConnectionState::Connecting | BrowserConnectionState::Open => {
                ReceiveOutcome::Empty
            }
            BrowserConnectionState::Disconnected => ReceiveOutcome::Disconnected,
            BrowserConnectionState::Failed(kind) => ReceiveOutcome::IoError(kind),
            BrowserConnectionState::Oversized(observed_at_least) => {
                ReceiveOutcome::Oversized { observed_at_least }
            }
        }
    }

    fn push_bytes(&mut self, bytes: &[u8]) -> Result<(), ()> {
        if bytes.len() > MAX_AFC_DATAGRAM_BYTES {
            self.metrics.oversized_frames = self.metrics.oversized_frames.saturating_add(1);
            self.connection = BrowserConnectionState::Oversized(bytes.len());
            return Err(());
        }
        if self.inbound.len() >= self.config.inbound_capacity_packets {
            self.metrics.inbound_queue_full = self.metrics.inbound_queue_full.saturating_add(1);
            self.connection = BrowserConnectionState::Failed(io::ErrorKind::OutOfMemory);
            return Err(());
        }
        let datagram = AfcDatagram::try_from_slice(bytes).map_err(|_| ())?;
        self.inbound.push_back(datagram);
        self.metrics.inbound_packets = self.metrics.inbound_packets.saturating_add(1);
        self.metrics.inbound_depth_packets = self.inbound.len();
        self.metrics.inbound_high_water_packets = self
            .metrics
            .inbound_high_water_packets
            .max(self.inbound.len());
        Ok(())
    }

    fn fail(&mut self, kind: io::ErrorKind) {
        self.metrics.transport_errors = self.metrics.transport_errors.saturating_add(1);
        self.connection = BrowserConnectionState::Failed(kind);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowserTransportPreference {
    WebTransportPreferred,
    WebTransportOnly,
    WebSocketOnly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowserWebEndpointBuildError {
    InvalidConfig(WebEndpointConfigError),
    InvalidAdmissionTicket,
    AdmissionRejected,
    WebSocketUnavailable,
    WebTransportUnavailable,
    WebTransportConnection,
    WebTransportDatagramsUnavailable,
    WebTransportDatagramCeiling { negotiated: u32 },
    WebTransportStream,
    NoSupportedTransport,
}

impl fmt::Display for BrowserWebEndpointBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "browser AFC endpoint could not connect: {self:?}"
        )
    }
}

impl std::error::Error for BrowserWebEndpointBuildError {}

pub struct BrowserWebSocketEndpoint {
    socket: WebSocket,
    state: Rc<RefCell<BrowserInboundState>>,
    _on_open: Closure<dyn FnMut(Event)>,
    _on_message: Closure<dyn FnMut(MessageEvent)>,
    _on_error: Closure<dyn FnMut(Event)>,
    _on_close: Closure<dyn FnMut(Event)>,
    _admission_timeout: Option<Closure<dyn FnMut()>>,
}

impl BrowserWebSocketEndpoint {
    pub fn connect(
        url: &str,
        config: WebEndpointConfig,
    ) -> Result<Self, BrowserWebEndpointBuildError> {
        Self::connect_inner(url, config, None)
    }

    pub fn connect_admitted(
        url: &str,
        ticket: &str,
        config: WebEndpointConfig,
    ) -> Result<Self, BrowserWebEndpointBuildError> {
        let admission = crate::web_admission::encode_admission_request(ticket)
            .map_err(|_| BrowserWebEndpointBuildError::InvalidAdmissionTicket)?;
        Self::connect_inner(url, config, Some(admission))
    }

    fn connect_inner(
        url: &str,
        config: WebEndpointConfig,
        admission: Option<Vec<u8>>,
    ) -> Result<Self, BrowserWebEndpointBuildError> {
        config
            .validate()
            .map_err(BrowserWebEndpointBuildError::InvalidConfig)?;
        let socket = WebSocket::new_with_str(url, AFC_WEBSOCKET_SUBPROTOCOL)
            .map_err(|_| BrowserWebEndpointBuildError::WebSocketUnavailable)?;
        socket.set_binary_type(BinaryType::Arraybuffer);
        let state = Rc::new(RefCell::new(BrowserInboundState::new(
            config,
            admission.is_some(),
        )));
        let admission_timeout = if admission.is_some() {
            let timeout_state = Rc::clone(&state);
            let timeout_socket = socket.clone();
            let callback = Closure::wrap(Box::new(move || {
                let mut state = timeout_state.borrow_mut();
                state.admission_timeout_handle = None;
                if state.admission_pending {
                    state.fail(io::ErrorKind::TimedOut);
                    drop(state);
                    let _ = timeout_socket.close_with_code(1008);
                }
            }) as Box<dyn FnMut()>);
            let handle = web_sys::window()
                .ok_or(BrowserWebEndpointBuildError::WebSocketUnavailable)?
                .set_timeout_with_callback_and_timeout_and_arguments_0(
                    callback.as_ref().unchecked_ref(),
                    BROWSER_ADMISSION_TIMEOUT_MS,
                )
                .map_err(|_| BrowserWebEndpointBuildError::WebSocketUnavailable)?;
            state.borrow_mut().admission_timeout_handle = Some(handle);
            Some(callback)
        } else {
            None
        };

        let open_state = Rc::clone(&state);
        let open_socket = socket.clone();
        let on_open = Closure::wrap(Box::new(move |_event: Event| {
            let Some(admission) = admission.as_ref() else {
                open_state.borrow_mut().connection = BrowserConnectionState::Open;
                return;
            };
            if open_socket.send_with_u8_array(admission).is_err() {
                open_state
                    .borrow_mut()
                    .fail(io::ErrorKind::PermissionDenied);
                let _ = open_socket.close_with_code(1008);
            }
        }) as Box<dyn FnMut(Event)>);
        socket.set_onopen(Some(on_open.as_ref().unchecked_ref()));

        let message_state = Rc::clone(&state);
        let message_socket = socket.clone();
        let on_message = Closure::wrap(Box::new(move |event: MessageEvent| {
            let data = event.data();
            let Ok(buffer) = data.dyn_into::<ArrayBuffer>() else {
                let mut state = message_state.borrow_mut();
                state.metrics.malformed_frames = state.metrics.malformed_frames.saturating_add(1);
                state.fail(io::ErrorKind::InvalidData);
                drop(state);
                let _ = message_socket.close_with_code(1003);
                return;
            };
            let view = Uint8Array::new(&buffer);
            let mut bytes = vec![0; view.length() as usize];
            view.copy_to(&mut bytes);
            if message_state.borrow().admission_pending {
                let mut state = message_state.borrow_mut();
                if crate::web_admission::is_admission_accepted(&bytes) {
                    state.admission_pending = false;
                    state.connection = BrowserConnectionState::Open;
                    clear_admission_timeout(&mut state);
                } else {
                    state.metrics.malformed_frames =
                        state.metrics.malformed_frames.saturating_add(1);
                    state.fail(io::ErrorKind::PermissionDenied);
                    clear_admission_timeout(&mut state);
                    drop(state);
                    let _ = message_socket.close_with_code(1008);
                }
                return;
            }
            if message_state.borrow_mut().push_bytes(&bytes).is_err() {
                let _ = message_socket.close_with_code(1009);
            }
        }) as Box<dyn FnMut(MessageEvent)>);
        socket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));

        let error_state = Rc::clone(&state);
        let on_error = Closure::wrap(Box::new(move |_event: Event| {
            let mut state = error_state.borrow_mut();
            state.fail(io::ErrorKind::ConnectionAborted);
            clear_admission_timeout(&mut state);
        }) as Box<dyn FnMut(Event)>);
        socket.set_onerror(Some(on_error.as_ref().unchecked_ref()));

        let close_state = Rc::clone(&state);
        let on_close = Closure::wrap(Box::new(move |_event: Event| {
            let mut state = close_state.borrow_mut();
            if matches!(
                state.connection,
                BrowserConnectionState::Connecting | BrowserConnectionState::Open
            ) {
                state.connection = BrowserConnectionState::Disconnected;
            }
            clear_admission_timeout(&mut state);
        }) as Box<dyn FnMut(Event)>);
        socket.set_onclose(Some(on_close.as_ref().unchecked_ref()));

        Ok(Self {
            socket,
            state,
            _on_open: on_open,
            _on_message: on_message,
            _on_error: on_error,
            _on_close: on_close,
            _admission_timeout: admission_timeout,
        })
    }

    pub fn metrics(&self) -> WebEndpointMetrics {
        self.state.borrow().metrics
    }
}

impl NonBlockingDatagramEndpoint for BrowserWebSocketEndpoint {
    fn try_send(&mut self, datagram: AfcDatagram) -> SendOutcome {
        match self.state.borrow().connection {
            BrowserConnectionState::Connecting => return SendOutcome::Full(datagram),
            BrowserConnectionState::Open => {}
            BrowserConnectionState::Disconnected | BrowserConnectionState::Oversized(_) => {
                return SendOutcome::Disconnected(datagram);
            }
            BrowserConnectionState::Failed(kind) => {
                return SendOutcome::IoError { datagram, kind };
            }
        }
        match self.socket.ready_state() {
            WebSocket::CONNECTING => SendOutcome::Full(datagram),
            WebSocket::OPEN => {
                let projected = self
                    .socket
                    .buffered_amount()
                    .saturating_add(datagram.len() as u32);
                if projected > self.state.borrow().config.websocket_buffered_bytes {
                    let mut state = self.state.borrow_mut();
                    state.metrics.outbound_queue_full =
                        state.metrics.outbound_queue_full.saturating_add(1);
                    return SendOutcome::Full(datagram);
                }
                match self.socket.send_with_u8_array(datagram.as_slice()) {
                    Ok(()) => {
                        let mut state = self.state.borrow_mut();
                        state.metrics.outbound_packets =
                            state.metrics.outbound_packets.saturating_add(1);
                        SendOutcome::Sent
                    }
                    Err(_) => {
                        self.state.borrow_mut().fail(io::ErrorKind::BrokenPipe);
                        SendOutcome::IoError {
                            datagram,
                            kind: io::ErrorKind::BrokenPipe,
                        }
                    }
                }
            }
            WebSocket::CLOSING | WebSocket::CLOSED => SendOutcome::Disconnected(datagram),
            _ => SendOutcome::IoError {
                datagram,
                kind: io::ErrorKind::InvalidData,
            },
        }
    }

    fn try_receive(&mut self) -> ReceiveOutcome {
        self.state.borrow_mut().receive()
    }
}

impl Drop for BrowserWebSocketEndpoint {
    fn drop(&mut self) {
        self.socket.set_onopen(None);
        self.socket.set_onmessage(None);
        self.socket.set_onerror(None);
        self.socket.set_onclose(None);
        let _ = self.socket.close_with_code(1000);
        let mut state = self.state.borrow_mut();
        clear_admission_timeout(&mut state);
        state.connection = BrowserConnectionState::Disconnected;
    }
}

fn clear_admission_timeout(state: &mut BrowserInboundState) {
    if let Some(handle) = state.admission_timeout_handle.take()
        && let Some(window) = web_sys::window()
    {
        window.clear_timeout_with_handle(handle);
    }
}

struct WebTransportState {
    inbound: BrowserInboundState,
    outbound: VecDeque<AfcDatagram>,
    writer: WritableStreamDefaultWriter,
    write_in_flight: bool,
    transport: JsWebTransport,
}

pub struct BrowserWebTransportEndpoint {
    state: Rc<RefCell<WebTransportState>>,
}

struct WebTransportConnectDeadline {
    handle: i32,
    callback: Closure<dyn FnMut()>,
}

impl WebTransportConnectDeadline {
    fn arm(transport: JsWebTransport) -> Result<Self, BrowserWebEndpointBuildError> {
        let callback_transport = transport;
        let callback =
            Closure::wrap(Box::new(move || callback_transport.close()) as Box<dyn FnMut()>);
        let handle = web_sys::window()
            .ok_or(BrowserWebEndpointBuildError::WebTransportUnavailable)?
            .set_timeout_with_callback_and_timeout_and_arguments_0(
                callback.as_ref().unchecked_ref(),
                BROWSER_ADMISSION_TIMEOUT_MS,
            )
            .map_err(|_| BrowserWebEndpointBuildError::WebTransportUnavailable)?;
        Ok(Self { handle, callback })
    }
}

impl Drop for WebTransportConnectDeadline {
    fn drop(&mut self) {
        if let Some(window) = web_sys::window() {
            window.clear_timeout_with_handle(self.handle);
        }
        let _ = &self.callback;
    }
}

impl BrowserWebTransportEndpoint {
    pub async fn connect(
        url: &str,
        config: WebEndpointConfig,
    ) -> Result<Self, BrowserWebEndpointBuildError> {
        Self::connect_inner(url, config, None).await
    }

    pub async fn connect_admitted(
        url: &str,
        ticket: &str,
        config: WebEndpointConfig,
    ) -> Result<Self, BrowserWebEndpointBuildError> {
        let admission = crate::web_admission::encode_admission_request(ticket)
            .map_err(|_| BrowserWebEndpointBuildError::InvalidAdmissionTicket)?;
        Self::connect_inner(url, config, Some(admission)).await
    }

    async fn connect_inner(
        url: &str,
        config: WebEndpointConfig,
        admission: Option<Vec<u8>>,
    ) -> Result<Self, BrowserWebEndpointBuildError> {
        config
            .validate()
            .map_err(BrowserWebEndpointBuildError::InvalidConfig)?;
        if !webtransport_is_available() {
            return Err(BrowserWebEndpointBuildError::WebTransportUnavailable);
        }
        let transport = JsWebTransport::new(url)
            .map_err(|_| BrowserWebEndpointBuildError::WebTransportUnavailable)?;
        let _deadline = WebTransportConnectDeadline::arm(transport.clone())?;
        if JsFuture::from(transport.ready()).await.is_err() {
            transport.close();
            return Err(BrowserWebEndpointBuildError::WebTransportConnection);
        }
        if let Some(admission) = admission
            && perform_webtransport_admission(&transport, &admission)
                .await
                .is_err()
        {
            transport.close();
            return Err(BrowserWebEndpointBuildError::AdmissionRejected);
        }
        let datagrams = transport.datagrams();
        let negotiated = datagrams.max_datagram_size();
        if negotiated == 0 {
            transport.close();
            return Err(BrowserWebEndpointBuildError::WebTransportDatagramsUnavailable);
        }
        if negotiated < MAX_AFC_DATAGRAM_BYTES as u32 {
            transport.close();
            return Err(BrowserWebEndpointBuildError::WebTransportDatagramCeiling { negotiated });
        }
        let reader = match ReadableStreamDefaultReader::new(&datagrams.readable()) {
            Ok(reader) => reader,
            Err(_) => {
                transport.close();
                return Err(BrowserWebEndpointBuildError::WebTransportStream);
            }
        };
        let writable = datagrams
            .create_writable()
            .unwrap_or_else(|_| datagrams.writable());
        let writer = match WritableStreamDefaultWriter::new(&writable) {
            Ok(writer) => writer,
            Err(_) => {
                reader.release_lock();
                transport.close();
                return Err(BrowserWebEndpointBuildError::WebTransportStream);
            }
        };
        let mut inbound = BrowserInboundState::new(config, false);
        inbound.connection = BrowserConnectionState::Open;
        let state = Rc::new(RefCell::new(WebTransportState {
            inbound,
            outbound: VecDeque::with_capacity(config.outbound_capacity_packets),
            writer,
            write_in_flight: false,
            transport: transport.clone(),
        }));
        spawn_webtransport_reader(reader, Rc::clone(&state));
        spawn_webtransport_closed(transport, Rc::clone(&state));
        Ok(Self { state })
    }

    pub fn metrics(&self) -> WebEndpointMetrics {
        self.state.borrow().inbound.metrics
    }
}

async fn perform_webtransport_admission(
    transport: &JsWebTransport,
    admission: &[u8],
) -> Result<(), ()> {
    let stream = JsFuture::from(transport.create_bidirectional_stream())
        .await
        .map_err(|_| ())?
        .dyn_into::<JsWebTransportBidirectionalStream>()
        .map_err(|_| ())?;
    let reader = ReadableStreamDefaultReader::new(&stream.readable()).map_err(|_| ())?;
    let writer = WritableStreamDefaultWriter::new(&stream.writable()).map_err(|_| ())?;
    JsFuture::from(writer.ready()).await.map_err(|_| ())?;
    let request = Uint8Array::from(admission);
    JsFuture::from(writer.write_with_chunk(request.as_ref()))
        .await
        .map_err(|_| ())?;
    JsFuture::from(writer.close()).await.map_err(|_| ())?;

    let mut response = Vec::with_capacity(crate::web_admission::ADMISSION_ACCEPTED_FRAME.len());
    while response.len() < crate::web_admission::ADMISSION_ACCEPTED_FRAME.len() {
        let value = JsFuture::from(reader.read()).await.map_err(|_| ())?;
        let done = Reflect::get(&value, &JsValue::from_str("done"))
            .ok()
            .and_then(|done| done.as_bool())
            .unwrap_or(true);
        if done {
            reader.release_lock();
            return Err(());
        }
        let chunk = Reflect::get(&value, &JsValue::from_str("value")).map_err(|_| ())?;
        let view = Uint8Array::new(&chunk);
        let old_len = response.len();
        let chunk_len = view.length() as usize;
        if chunk_len == 0
            || old_len.saturating_add(chunk_len)
                > crate::web_admission::ADMISSION_ACCEPTED_FRAME.len()
        {
            reader.release_lock();
            return Err(());
        }
        response.resize(old_len + chunk_len, 0);
        view.copy_to(&mut response[old_len..]);
    }
    let accepted = crate::web_admission::is_admission_accepted(&response);
    let _ = JsFuture::from(reader.cancel()).await;
    reader.release_lock();
    accepted.then_some(()).ok_or(())
}

impl NonBlockingDatagramEndpoint for BrowserWebTransportEndpoint {
    fn try_send(&mut self, datagram: AfcDatagram) -> SendOutcome {
        {
            let mut state = self.state.borrow_mut();
            match state.inbound.connection {
                BrowserConnectionState::Open => {}
                BrowserConnectionState::Connecting => return SendOutcome::Full(datagram),
                BrowserConnectionState::Disconnected | BrowserConnectionState::Oversized(_) => {
                    return SendOutcome::Disconnected(datagram);
                }
                BrowserConnectionState::Failed(kind) => {
                    return SendOutcome::IoError { datagram, kind };
                }
            }
            if state.outbound.len() >= state.inbound.config.outbound_capacity_packets {
                state.inbound.metrics.outbound_queue_full =
                    state.inbound.metrics.outbound_queue_full.saturating_add(1);
                return SendOutcome::Full(datagram);
            }
            state.outbound.push_back(datagram);
            state.inbound.metrics.outbound_depth_packets = state.outbound.len();
            state.inbound.metrics.outbound_high_water_packets = state
                .inbound
                .metrics
                .outbound_high_water_packets
                .max(state.outbound.len());
        }
        kick_webtransport_writer(Rc::clone(&self.state));
        SendOutcome::Sent
    }

    fn try_receive(&mut self) -> ReceiveOutcome {
        self.state.borrow_mut().inbound.receive()
    }
}

impl Drop for BrowserWebTransportEndpoint {
    fn drop(&mut self) {
        let mut state = self.state.borrow_mut();
        state.inbound.connection = BrowserConnectionState::Disconnected;
        state.outbound.clear();
        state.transport.close();
    }
}

fn kick_webtransport_writer(state: Rc<RefCell<WebTransportState>>) {
    let (writer, datagram) = {
        let mut state_ref = state.borrow_mut();
        if state_ref.write_in_flight || state_ref.inbound.connection != BrowserConnectionState::Open
        {
            return;
        }
        let Some(datagram) = state_ref.outbound.pop_front() else {
            return;
        };
        state_ref.write_in_flight = true;
        state_ref.inbound.metrics.outbound_depth_packets = state_ref.outbound.len();
        (state_ref.writer.clone(), datagram)
    };
    spawn_local(async move {
        let ready = JsFuture::from(writer.ready()).await;
        let chunk = Uint8Array::from(datagram.as_slice());
        let written = if ready.is_ok() {
            JsFuture::from(writer.write_with_chunk(chunk.as_ref())).await
        } else {
            ready
        };
        {
            let mut state_ref = state.borrow_mut();
            state_ref.write_in_flight = false;
            if written.is_ok() {
                state_ref.inbound.metrics.outbound_packets =
                    state_ref.inbound.metrics.outbound_packets.saturating_add(1);
            } else {
                state_ref.inbound.fail(io::ErrorKind::BrokenPipe);
                state_ref.outbound.clear();
                state_ref.inbound.metrics.outbound_depth_packets = 0;
                state_ref.transport.close();
            }
        }
        if written.is_ok() {
            kick_webtransport_writer(state);
        }
    });
}

fn spawn_webtransport_reader(
    reader: ReadableStreamDefaultReader,
    state: Rc<RefCell<WebTransportState>>,
) {
    spawn_local(async move {
        loop {
            let value = match JsFuture::from(reader.read()).await {
                Ok(value) => value,
                Err(_) => {
                    let mut state_ref = state.borrow_mut();
                    state_ref.inbound.fail(io::ErrorKind::ConnectionAborted);
                    state_ref.transport.close();
                    break;
                }
            };
            let done = Reflect::get(&value, &JsValue::from_str("done"))
                .ok()
                .and_then(|done| done.as_bool())
                .unwrap_or(true);
            if done {
                let mut state_ref = state.borrow_mut();
                if state_ref.inbound.connection == BrowserConnectionState::Open {
                    state_ref.inbound.connection = BrowserConnectionState::Disconnected;
                }
                break;
            }
            let Ok(chunk) = Reflect::get(&value, &JsValue::from_str("value")) else {
                let mut state_ref = state.borrow_mut();
                state_ref.inbound.fail(io::ErrorKind::InvalidData);
                state_ref.transport.close();
                break;
            };
            let view = Uint8Array::new(&chunk);
            let mut bytes = vec![0; view.length() as usize];
            view.copy_to(&mut bytes);
            if state.borrow_mut().inbound.push_bytes(&bytes).is_err() {
                state.borrow().transport.close();
                break;
            }
        }
        reader.release_lock();
    });
}

fn spawn_webtransport_closed(transport: JsWebTransport, state: Rc<RefCell<WebTransportState>>) {
    spawn_local(async move {
        let _ = JsFuture::from(transport.closed()).await;
        let mut state_ref = state.borrow_mut();
        if matches!(
            state_ref.inbound.connection,
            BrowserConnectionState::Connecting | BrowserConnectionState::Open
        ) {
            state_ref.inbound.connection = BrowserConnectionState::Disconnected;
        }
    });
}

fn webtransport_is_available() -> bool {
    js_sys::global()
        .dyn_into::<js_sys::Object>()
        .ok()
        .is_some_and(|global| {
            Reflect::has(&global, &JsValue::from_str("WebTransport")).unwrap_or(false)
        })
}

pub enum BrowserDatagramEndpoint {
    WebTransport(BrowserWebTransportEndpoint),
    WebSocket(BrowserWebSocketEndpoint),
}

impl BrowserDatagramEndpoint {
    pub const fn kind(&self) -> WebEndpointKind {
        match self {
            Self::WebTransport(_) => WebEndpointKind::WebTransport,
            Self::WebSocket(_) => WebEndpointKind::WebSocket,
        }
    }

    pub fn metrics(&self) -> WebEndpointMetrics {
        match self {
            Self::WebTransport(endpoint) => endpoint.metrics(),
            Self::WebSocket(endpoint) => endpoint.metrics(),
        }
    }
}

impl NonBlockingDatagramEndpoint for BrowserDatagramEndpoint {
    fn try_send(&mut self, datagram: AfcDatagram) -> SendOutcome {
        match self {
            Self::WebTransport(endpoint) => endpoint.try_send(datagram),
            Self::WebSocket(endpoint) => endpoint.try_send(datagram),
        }
    }

    fn try_receive(&mut self) -> ReceiveOutcome {
        match self {
            Self::WebTransport(endpoint) => endpoint.try_receive(),
            Self::WebSocket(endpoint) => endpoint.try_receive(),
        }
    }
}

pub async fn connect_browser_datagram_endpoint(
    preference: BrowserTransportPreference,
    webtransport_url: &str,
    websocket_url: &str,
    config: WebEndpointConfig,
) -> Result<BrowserDatagramEndpoint, BrowserWebEndpointBuildError> {
    config
        .validate()
        .map_err(BrowserWebEndpointBuildError::InvalidConfig)?;
    match preference {
        BrowserTransportPreference::WebSocketOnly => {
            BrowserWebSocketEndpoint::connect(websocket_url, config)
                .map(BrowserDatagramEndpoint::WebSocket)
        }
        BrowserTransportPreference::WebTransportOnly => {
            BrowserWebTransportEndpoint::connect(webtransport_url, config)
                .await
                .map(BrowserDatagramEndpoint::WebTransport)
        }
        BrowserTransportPreference::WebTransportPreferred => {
            if let Ok(endpoint) =
                BrowserWebTransportEndpoint::connect(webtransport_url, config).await
            {
                return Ok(BrowserDatagramEndpoint::WebTransport(endpoint));
            }
            BrowserWebSocketEndpoint::connect(websocket_url, config)
                .map(BrowserDatagramEndpoint::WebSocket)
                .map_err(|_| BrowserWebEndpointBuildError::NoSupportedTransport)
        }
    }
}

pub async fn connect_browser_admitted_datagram_endpoint(
    preference: BrowserTransportPreference,
    webtransport_url: &str,
    websocket_url: &str,
    ticket: &str,
    config: WebEndpointConfig,
) -> Result<BrowserDatagramEndpoint, BrowserWebEndpointBuildError> {
    config
        .validate()
        .map_err(BrowserWebEndpointBuildError::InvalidConfig)?;
    match preference {
        BrowserTransportPreference::WebSocketOnly => {
            BrowserWebSocketEndpoint::connect_admitted(websocket_url, ticket, config)
                .map(BrowserDatagramEndpoint::WebSocket)
        }
        BrowserTransportPreference::WebTransportOnly => {
            BrowserWebTransportEndpoint::connect_admitted(webtransport_url, ticket, config)
                .await
                .map(BrowserDatagramEndpoint::WebTransport)
        }
        BrowserTransportPreference::WebTransportPreferred => {
            let webtransport =
                BrowserWebTransportEndpoint::connect_admitted(webtransport_url, ticket, config)
                    .await;
            match webtransport {
                Ok(endpoint) => return Ok(BrowserDatagramEndpoint::WebTransport(endpoint)),
                // These failures happen before the one-time ticket is placed
                // on an admission stream, so trying WebSocket cannot replay a
                // ticket that the authority may already have consumed.
                Err(BrowserWebEndpointBuildError::WebTransportUnavailable)
                | Err(BrowserWebEndpointBuildError::WebTransportConnection) => {}
                Err(error) => return Err(error),
            }
            BrowserWebSocketEndpoint::connect_admitted(websocket_url, ticket, config)
                .map(BrowserDatagramEndpoint::WebSocket)
                .map_err(|_| BrowserWebEndpointBuildError::NoSupportedTransport)
        }
    }
}
