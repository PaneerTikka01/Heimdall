// src/itchy_parser.rs

use anyhow::Context;
use itchy::{Body, MessageStream, Side as ItchySide};
use crate::arbiter::{OrderEvent, Side};

pub fn parse_file(path: &str) -> anyhow::Result<impl Iterator<Item = OrderEvent>> {
    let stream = MessageStream::from_file(path)
        .with_context(|| format!("failed to open ITCH file '{}'", path))?;

    let iter = stream.filter_map(|res_msg| {
        let msg = match res_msg {
            Ok(m) => m,
            Err(_) => return None,
        };

        let timestamp: u64 = msg.timestamp;

        match msg.body {
            Body::AddOrder(a) => {
                let my_side = match a.side {
                    ItchySide::Buy  => Side::Buy,
                    ItchySide::Sell => Side::Sell,
                };
                Some(OrderEvent::New {
                    timestamp,
                    id:     a.reference,
                    symbol: a.stock.to_string(),
                    side:   my_side,
                    price:  a.price.raw(),
                    size:   a.shares,
                })
            }
            Body::OrderCancelled { reference, cancelled } => {
                Some(OrderEvent::Cancel {
                    timestamp,
                    id:   reference,
                    size: cancelled,
                })
            }
            Body::ReplaceOrder(r) => {
                Some(OrderEvent::Replace {
                    timestamp,
                    old_id:    r.old_reference,
                    new_id:    r.new_reference,
                    new_size:  r.shares,
                    new_price: r.price.raw(),
                })
            }
            _ => None,
        }
    });

    Ok(iter)
}
