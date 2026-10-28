//! The receiving side: asks the person about offers through the UI.

use crate::format;
use crate::state::{App, PendingOffer, Transfer};
use crate::text;
use yeet_core::recv::{Handler, Offer};
use yeet_core::{Live, Report, Result};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long the person has to answer an incoming offer.
const ANSWER_WITHIN: Duration = Duration::from_secs(60);
/// Item names listed in the offer sheet before "and N more".
const SHOWN_ITEMS: usize = 4;

pub struct Receiver(pub Arc<App>);

impl Handler for Receiver {
    fn offer(&self, offer: &Offer, withdrawn: &AtomicBool) -> Option<PathBuf> {
        #[cfg(feature = "e2e")]
        if crate::e2e::auto_accept() {
            return Some(self.0.state().settings.save_dir.clone());
        }
        let (reply, answer) = crossbeam_channel::bounded(1);
        {
            let mut st = self.0.state();
            if st.offer.is_some() {
                return None; // already asking about another one
            }
            let mut items: Vec<String> = offer.items.iter().take(SHOWN_ITEMS).cloned().collect();
            if offer.items.len() > SHOWN_ITEMS {
                items.push(format!("and {} more", offer.items.len() - SHOWN_ITEMS));
            }
            st.offer = Some(PendingOffer {
                from: offer.name.clone(),
                summary: format!("{} · {}", format::files(offer.files), format::bytes(offer.bytes)),
                items,
                reply,
            });
        }
        let deadline = Instant::now() + ANSWER_WITHIN;
        let accepted = loop {
            if withdrawn.load(Relaxed) {
                break false;
            }
            match answer.recv_timeout(Duration::from_millis(100)) {
                Ok(a) => break a,
                Err(crossbeam_channel::RecvTimeoutError::Timeout) if Instant::now() < deadline => {}
                Err(_) => break false,
            }
        };
        let mut st = self.0.state();
        st.offer = None;
        if withdrawn.load(Relaxed) {
            st.toast = Some((format!("{} stopped sending.", offer.name), Instant::now()));
        }
        accepted.then(|| st.settings.save_dir.clone())
    }

    fn started(&self, offer: &Offer, live: Arc<Live>) {
        let t = Transfer::new(offer.transfer, true, offer.name.clone(), text::what(&offer.items), live);
        self.0.state().transfers.insert(0, t);
    }

    fn finished(&self, offer: &Offer, result: &Result<Report>, saved: &[PathBuf]) {
        let mut st = self.0.state();
        let running = st.transfers.iter_mut().find(|t| t.incoming && t.key == offer.transfer && t.running());
        if let Some(t) = running {
            t.outcome = Some(match result {
                Ok(r) => Ok(r.clone()),
                Err(e) => Err(text::describe(e, &offer.name, true)),
            });
            t.saved = saved.to_vec();
        }
    }
}
