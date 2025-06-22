//src/arbiter.rs

use std::collections::{BTreeMap, HashMap, VecDeque};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side { Buy, Sell }

#[derive(Debug, Clone)]
pub enum OrderEvent {
    New     { timestamp: u64, id: u64, symbol: String, side: Side, price: u32, size: u32 },
    Cancel  { timestamp: u64, id: u64, size: u32 },
    Replace { timestamp: u64, old_id: u64, new_id: u64, new_size: u32, new_price: u32 },
}

#[derive(Debug, Clone)]
pub struct Order {
    id:        u64,
    side:      Side,
    price:     u32,
    size:      u32,
    timestamp: u64, 
}

/// Per-symbol limit order book.
pub struct OrderBook {
    // price -> FIFO queue of orders at that price
    bids:  BTreeMap<u32, VecDeque<Order>>, // buy side: highest price matched first
    asks:  BTreeMap<u32, VecDeque<Order>>, // sell side: lowest price matched first
    // index: order_id -> (Side, price). For locating orders to cancel/execute.
    index: HashMap<u64, (Side, u32)>,
}

impl OrderBook {
    pub fn new() -> Self {
        Self {
            bids:  BTreeMap::new(),
            asks:  BTreeMap::new(),
            index: HashMap::new(),
        }
    }

    /// Match an incoming limit order against resting orders on the opposite side,
    /// in price-time priority (best price first, FIFO within same price). If leftover remains,
    /// insert into this side’s book.
    pub fn match_limit(&mut self, mut incoming: Order) {
        let mut remaining = incoming.size;

        match incoming.side {
            Side::Buy => {
                // While incoming has remaining and best ask price <= incoming.price
                loop {
                    // Find lowest ask price
                    let best_price_opt = {
                        let mut iter = self.asks.iter();
                        iter.next().map(|(&price, _)| price)
                    };
                    let price = match best_price_opt {
                        Some(p) if p <= incoming.price => p,
                        _ => break,
                    };
                    if let Some(queue) = self.asks.get_mut(&price) {

                        while remaining > 0 && !queue.is_empty() {
                            let resting = queue.front_mut().unwrap();
                            let trade_qty = remaining.min(resting.size);

                            resting.size -= trade_qty;
                            remaining    -= trade_qty;
                            if resting.size == 0 {
                                let filled = queue.pop_front().unwrap();
                                self.index.remove(&filled.id);
                            }
                        }
                        if queue.is_empty() {
                            self.asks.remove(&price);
                        }
                        if remaining == 0 {
                            break;
                        }
                    } else {
                        break;
                    }
                }
                // Insert leftover as resting buy
                if remaining > 0 {
                    incoming.size = remaining;
                    let level = self.bids.entry(incoming.price).or_default();
                    level.push_back(incoming.clone());
                    self.index.insert(incoming.id, (incoming.side, incoming.price));
                }
            }
            Side::Sell => {
                // While incoming has remaining and best bid price >= incoming.price
                loop {
                    // Find highest bid price
                    let best_price_opt = {
                        let mut iter = self.bids.iter();
                        iter.next_back().map(|(&price, _)| price)
                    };
                    let price = match best_price_opt {
                        Some(p) if p >= incoming.price => p,
                        _ => break,
                    };
                    if let Some(queue) = self.bids.get_mut(&price) {
                        while remaining > 0 && !queue.is_empty() {
                            let resting = queue.front_mut().unwrap();
                            let trade_qty = remaining.min(resting.size);
                            resting.size -= trade_qty;
                            remaining    -= trade_qty;
                            if resting.size == 0 {
                                let filled = queue.pop_front().unwrap();
                                self.index.remove(&filled.id);
                            }
                        }
                        if queue.is_empty() {
                            self.bids.remove(&price);
                        }
                        if remaining == 0 {
                            break;
                        }
                    } else {
                        break;
                    }
                }
                // Insert leftover as resting sell
                if remaining > 0 {
                    incoming.size = remaining;
                    let level = self.asks.entry(incoming.price).or_default();
                    level.push_back(incoming.clone());
                    self.index.insert(incoming.id, (incoming.side, incoming.price));
                }
            }
        }
    }

    /// Handle a Cancel event for this book. Returns true if order was fully removed.
    pub fn handle_cancel(&mut self, id: u64, size: u32) -> bool {
        if let Some(&(side, price)) = self.index.get(&id) {
            let book_side = if side == Side::Buy { &mut self.bids } else { &mut self.asks };
            if let Some(queue) = book_side.get_mut(&price) {
                queue.retain_mut(|o| {
                    if o.id == id {
                        if size >= o.size {
                            // fully cancel
                            false
                        } else {
                            // partial cancel
                            o.size -= size;
                            true
                        }
                    } else {
                        true
                    }
                });
                if queue.is_empty() {
                    book_side.remove(&price);
                }
            }
            // Remove from index if no longer present
            if let Some(&(s, p)) = self.index.get(&id) {
                // Check if still in queue
                let exists = if s == Side::Buy {
                    self.bids.get(&p).map_or(false, |q| q.iter().any(|o| o.id == id))
                } else {
                    self.asks.get(&p).map_or(false, |q| q.iter().any(|o| o.id == id))
                };
                if !exists {
                    self.index.remove(&id);
                    return true;
                }
            } else {
                // already removed
                return true;
            }
        }
        false
    }


    
}

/// engine managing multiple symbols and global ID -> symbol mapping.
pub struct MatchingEngine {
    // symbol -> OrderBook
    books:  HashMap<String, OrderBook>,
    // global mapping: order ID -> (symbol, side)
    id_map: HashMap<u64, (String, Side)>,
    stats:  EngineStats,
}

struct EngineStats {
    total_new:      u64,
    total_cancel:   u64,
    total_replace:  u64,
}

impl MatchingEngine {
    pub fn new() -> Self {
        Self {
            books:  HashMap::new(),
            id_map: HashMap::new(),
            stats: EngineStats {
                total_new: 0,
                total_cancel: 0,
                total_replace: 0,
            },
        }
    }

    /// Handle an OrderEvent, routing by symbol using id_map so that only same-symbol orders match.
    pub fn handle(&mut self, ev: OrderEvent) {
        // Update stats
        match &ev {
            OrderEvent::New { .. }     => self.stats.total_new += 1,
            OrderEvent::Cancel { .. }  => self.stats.total_cancel += 1,
            OrderEvent::Replace { .. } => self.stats.total_replace += 1,
        }

        match ev {
            OrderEvent::New { timestamp, id, symbol, side, price, size } => {
                // Insert/match in the correct symbol book
                let book = self.books.entry(symbol.clone()).or_insert_with(OrderBook::new);
                // Build Order with timestamp (used for analytics; FIFO preserved by insertion order)
                let order = Order { id, side, price, size, timestamp };
                book.match_limit(order);
                // Record global mapping
                self.id_map.insert(id, (symbol, side));
            }

            OrderEvent::Cancel { timestamp: _, id, size } => {
                // Lookup symbol from id_map
                if let Some((symbol, _side)) = self.id_map.get(&id) {
                    if let Some(book) = self.books.get_mut(symbol) {
                        let fully_removed = book.handle_cancel(id, size);
                        if fully_removed {
                            self.id_map.remove(&id);
                        }
                    }
                }
            }

            OrderEvent::Replace { timestamp, old_id, new_id, new_size, new_price } => {
                // Lookup old order's symbol & side
                if let Some((symbol, side)) = self.id_map.get(&old_id).cloned() {
                    if let Some(book) = self.books.get_mut(&symbol) {
                        // Cancel old fully
                        let _ = book.handle_cancel(old_id, u32::MAX);
                        self.id_map.remove(&old_id);
                        // Insert new order with same symbol & side
                        let order = Order { id: new_id, side, price: new_price, size: new_size, timestamp };
                        book.match_limit(order);
                        self.id_map.insert(new_id, (symbol, side));
                    }
                }
            }
        }
    }

    /// Prints the results of my matching engine
    pub fn print_stats(&self) {
        println!("Orderbook Statistics:");
        println!("  Total New Orders:      {}", self.stats.total_new);
        println!("  Total Cancel Events:   {}", self.stats.total_cancel);
        println!("  Total Replace Events:  {}", self.stats.total_replace);
    }
}
