# Heimdall

Heimdall is a high-throughput ITCH feed parser and per-symbol limit order matching engine written in Rust. Named after the vigilant guardian of the Bifröst in Norse mythology, it “oversees” the entire flow from raw NASDAQ ITCH messages to maintaining LOBs (Limit Order Books) and executing trades in price-time priority. Heimdall leverages the [`itchy`](https://crates.io/crates/itchy) crate for parsing and uses efficient in-memory data structures (`BTreeMap`, `VecDeque`, `HashMap`) to achieve multi-million messages-per-second throughput.


---

## Overview

Heimdall is designed to:
- **Parse** raw NASDAQ ITCH feed messages via the `itchy` crate.  
- **Maintain** per-symbol limit order books (LOBs) in-memory.  
- **Match** incoming orders (New/Cancel/Replace events) in **price-time priority** (best price first, FIFO within same price level).  
- **Route** events correctly using a global ID->symbol mapping so that only orders of the same symbol interact.  
- **Achieve** multi-million messages-per-second throughput on typical hardware in release mode.  


---

## Key Terms: 

- **ITCH Feed**: A high-performance exchange data feed protocol (e.g., NASDAQ ITCH) delivering order adds, cancels, executions, replaces, trades, etc.  
- **`itchy` crate**: A Rust library for decoding ITCH messages into a Rust-friendly API (`MessageStream`, `Body::AddOrder`, etc.).  
- **LOB (Limit Order Book)**: The data structure holding resting buy (bids) and sell (asks) limit orders for a given symbol.  
- **Price-Time Priority**: Matching rule where orders at the best price get executed first; within the same price level, orders are FIFO (first-in, first-out) by arrival time.  
- **Throughput**: Number of messages processed per second (e.g., millions msg/sec).  
- **Latency**: Time to process a single event or match; Heimdall is optimized for low processing latency in a backtest/replay context.  
- **Data Structures**:  
  - `BTreeMap<u32, VecDeque<Order>>` for sorted price levels + FIFO queue per price.  
  - `HashMap<u64, (Side, u32)> index` within each book to locate resting orders by ID.  
  - `HashMap<String, OrderBook> books` for per-symbol books.  
  - `HashMap<u64, (String, Side)> id_map` for global order ID → symbol & side routing.  
- **OrderEvent**: Enum representing parsed events: `New`, `Cancel`, `Replace` (we drop `Execute` if unused).  
- **match_limit**: Core matching function in `OrderBook` that executes incoming limit orders against resting opposite-side orders.  
- **id_map**: Global mapping so Cancel/Replace events (which lack symbol in parsed data) are routed to the correct `OrderBook`.  

---

## Architecture & Components

### Parser (`itchy_parser.rs`)

- Uses `itchy::MessageStream::from_file(path)` to open and iterate raw ITCH messages.  
- Filters only relevant `Body` variants:
  - `Body::AddOrder(a)` → `OrderEvent::New { timestamp, id, symbol: a.stock.to_string(), side, price, size }`.  
  - `Body::OrderCancelled { reference, cancelled }` → `OrderEvent::Cancel { timestamp, id: reference, size: cancelled }`.  
  - `Body::ReplaceOrder(r)` → `OrderEvent::Replace { timestamp, old_id, new_id, new_price, new_size }`.  
- Yields `Iterator<Item = OrderEvent>`.  

### OrderEvent & Data Model

- `pub enum OrderEvent { New { timestamp: u64, id: u64, symbol: String, side: Side, price: u32, size: u32 }, Cancel { timestamp: u64, id: u64, size: u32 }, Replace { timestamp: u64, old_id: u64, new_id: u64, new_size: u32, new_price: u32 }, }`.  
- `pub struct Order { id: u64, side: Side, price: u32, size: u32, timestamp: u64 }`.  
- `Side` enum: `Buy` or `Sell`.  
- FIFO ensured by `VecDeque<Order>` insertion order; timestamp field is recorded for analytics but not used in matching logic.

### Per-Symbol OrderBook (`arbiter.rs`)

- `pub struct OrderBook { bids: BTreeMap<u32, VecDeque<Order>>, asks: BTreeMap<u32, VecDeque<Order>>, index: HashMap<u64, (Side, u32)> }`.  
  - **bids**: key = price, value = FIFO queue of buy orders at that price. Highest bid matched first via `.iter().next_back()`.  
  - **asks**: key = price, FIFO queue of sell orders. Lowest ask matched first via `.iter().next()`.  
  - **index**: maps order ID to (Side, price) so cancels and replaces can locate and remove or update the resting order efficiently.  
- **match_limit(incoming: Order)**:
  1. Determine opposite side map (`asks` if incoming is Buy; `bids` if Sell).
  2. Loop: find best price level (lowest ask or highest bid).  
     - If best price satisfies limit condition, iterate FIFO queue at that price: subtract sizes, remove filled orders, update `index`.  
     - Remove empty price levels. Break when incoming size is zero or best price no longer matches.  
  3. If leftover remains, insert as a resting order on its side: `bids.entry(price).push_back(...)` or `asks.entry(price).push_back(...)`, record in `index`.
- **handle_cancel(id, size)**:
  - Lookup `(side, price)` in `index`. Get queue at that price, `retain_mut` to subtract or remove the order. Remove price level if empty; remove from `index` if fully removed.

### MatchingEngine & Global Routing

- `pub struct MatchingEngine { books: HashMap<String, OrderBook>, id_map: HashMap<u64, (String, Side)>, stats: EngineStats }`.  
  - **books**: per-symbol `OrderBook`.  
  - **id_map**: global order ID -> (symbol, side). Used so that Cancel/Replace (which lack symbol) can be routed correctly.  
  - **stats**: counters for total New, Cancel, Replace events processed.  
- **handle(ev: OrderEvent)**:
  1. Increment stats based on variant.  
  2. Match on `ev`:
     - `New { timestamp, id, symbol, side, price, size }`:  
       - `let book = books.entry(symbol.clone()).or_insert_with(OrderBook::new);`  
       - Create `Order { id, side, price, size, timestamp }`, call `book.match_limit(order)`.  
       - Insert `(id -> (symbol, side))` into `id_map`.  
     - `Cancel { timestamp, id, size }`:  
       - Lookup `(symbol, _)` from `id_map`, route to `books[symbol].handle_cancel(id, size)`. If fully removed, remove from `id_map`.  
     - `Replace { timestamp, old_id, new_id, new_size, new_price }`:  
       - Lookup `(symbol, side)` from `id_map` for `old_id`. Call `book.handle_cancel(old_id, u32::MAX)`, remove old_id from `id_map`.  
       - Create new `Order { id: new_id, side, price: new_price, size: new_size, timestamp }`, call `book.match_limit(order)`, insert `(new_id -> (symbol, side))` into `id_map`.
      

### How `main.rs` Works

- Uses a **two-pass design** to cleanly separate parsing and matching performance:
  
  1. **First Pass** (Parsing Only):
     - Opens the ITCH feed using the `itchy` crate.
     - Iterates through up to `MAX_MSG` events, counting them.
     - Measures **parse throughput** in messages per second.

  2. **Second Pass** (Matching Logic):
     - Re-runs the parser on the same file.
     - Feeds each event into the `MatchingEngine`, which:
       - Maintains a per-symbol **limit order book** (`OrderBook`)
       - Applies **price-time priority** matching
       - Handles `New`, `Cancel`, and `Replace` messages.
     - Measures **matching throughput** in messages per second.

- Finally, the program prints:
  - Number of processed messages
  - Parse time & speed
  - Matching time & speed
  - Count of each type of event via `engine.print_stats()`

### Why Two Passes?

This architecture isolates the performance of **ITCH feed decoding** from the **order matching engine**, making benchmarking more accurate and modular.

---

## Performance (1M messages)

```text
Processed 1000000 messages (max 1000000)
Test Device: Macbook Air M1 (8GB RAM,256GB Memory)

ITCH Parsing
  Parse Time:  0.121 secs
  Parse Speed: 8295961 msg/sec

LOB Matching
  Match Time:  0.377 secs
  Match Speed: 2654496 msg/sec

Orderbook Statistics:
  Total New Orders:      670500
  Total Cancel Events:   242432
  Total Replace Events:  87068



- **Parse Speed**: ~8.3M msg/sec via `itchy` crate.
- **Match Speed**: ~2.65M msg/sec for per-symbol LOB matching (price-time priority).
```

## Usage

1. **Clone the Repository**
   ```bash
   git clone https://github.com/PaneerTikka01/Heimdall.git
   cd Heimdall
2. **Download the ITCH 5.0 dataset from NASDAQ if you want a different dataset.**
3. Place the file in the root of the project directory.
4. Build the project and run the engine
5.  ```bash
     cargo build --release
    ./target/release/Heimdall


