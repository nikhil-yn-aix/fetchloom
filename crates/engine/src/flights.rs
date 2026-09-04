//! The two in-flight bounds a run holds every transfer inside.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use crate::error::Error;
use crate::tuning::{Ceilings, Controller};

pub struct Flights<'a> {
    ceilings: Ceilings,
    start: Box<dyn Fn(&str) -> Controller + Sync + 'a>,
    state: Mutex<State>,
    room: Condvar,
}

struct State {
    in_flight: u32,
    hosts: HashMap<String, InFlight>,
    pending: Vec<usize>,
    stop_above: Option<usize>,
}

struct InFlight {
    count: u32,
    controller: Arc<Mutex<Controller>>,
}

impl<'a> Flights<'a> {
    #[must_use]
    pub fn new(ceilings: Ceilings, start: impl Fn(&str) -> Controller + Sync + 'a) -> Self {
        Self {
            ceilings,
            start: Box::new(start),
            state: Mutex::new(State {
                in_flight: 0,
                hosts: HashMap::new(),
                pending: Vec::new(),
                stop_above: None,
            }),
            room: Condvar::new(),
        }
    }

    #[must_use]
    pub fn controller(&self, host: &str) -> Arc<Mutex<Controller>> {
        let mut state = self.locked();
        Arc::clone(&self.entry(&mut state, host).controller)
    }

    pub fn each<Item, Out>(
        &self,
        items: &[Item],
        hosts: &[String],
        job: &(dyn Fn(&Item) -> Result<Out, Error> + Sync),
    ) -> Produced<Out>
    where
        Item: Sync,
        Out: Send,
    {
        {
            let mut state = self.locked();
            state.pending = (0..items.len()).collect();
            state.stop_above = None;
        }
        let produced: Mutex<Vec<Option<Result<Out, Error>>>> =
            Mutex::new((0..items.len()).map(|_| None).collect());
        let workers = usize::try_from(self.ceilings.global.get())
            .unwrap_or(usize::MAX)
            .min(items.len().max(1));

        std::thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(|| {
                    while let Some(index) = self.take(hosts) {
                        let outcome = job(&items[index]);
                        self.release(&hosts[index], index, outcome.is_err());
                        let mut slot = produced.lock().unwrap_or_else(PoisonError::into_inner);
                        slot[index] = Some(outcome);
                    }
                });
            }
        });

        let mut outputs = Vec::with_capacity(items.len());
        for slot in produced
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner)
            .into_iter()
            .flatten()
        {
            match slot {
                Ok(output) => outputs.push(output),
                Err(failure) => {
                    return Produced {
                        outputs,
                        failure: Some(failure),
                    };
                }
            }
        }
        Produced {
            outputs,
            failure: None,
        }
    }

    fn take(&self, hosts: &[String]) -> Option<usize> {
        let mut state = self.locked();
        loop {
            if crate::cancel::requested() {
                return None;
            }
            if state.pending.is_empty() {
                return None;
            }
            if let Some(position) = self.with_room(&state, hosts) {
                let index = state.pending.remove(position);
                state.in_flight += 1;
                self.entry(&mut state, &hosts[index]).count += 1;
                return Some(index);
            }
            state = self
                .room
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    fn with_room(&self, state: &State, hosts: &[String]) -> Option<usize> {
        if state.in_flight >= self.ceilings.global.get() {
            return None;
        }
        state.pending.iter().position(|index| {
            state.hosts.get(&hosts[*index]).is_none_or(|held| {
                held.count
                    < held
                        .controller
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .permitted()
            })
        })
    }

    fn release(&self, host: &str, index: usize, failed: bool) {
        let mut state = self.locked();
        state.in_flight = state.in_flight.saturating_sub(1);
        self.entry(&mut state, host).count = self.entry(&mut state, host).count.saturating_sub(1);
        if failed {
            let stop = state.stop_above.map_or(index, |held| held.min(index));
            state.stop_above = Some(stop);
            state.pending.retain(|pending| *pending < stop);
        }
        drop(state);
        self.room.notify_all();
    }

    fn entry<'state>(&self, state: &'state mut State, host: &str) -> &'state mut InFlight {
        state
            .hosts
            .entry(host.to_owned())
            .or_insert_with(|| InFlight {
                count: 0,
                controller: Arc::new(Mutex::new((self.start)(host))),
            })
    }

    fn locked(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl std::fmt::Debug for Flights<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Flights")
            .field("ceilings", &self.ceilings)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub struct Produced<Out> {
    pub outputs: Vec<Out>,
    pub failure: Option<Error>,
}

impl Flights<'_> {
    #[must_use]
    pub fn headroom(&self, host: &str) -> u32 {
        let mut state = self.locked();
        let permitted = {
            let held = self.entry(&mut state, host);
            let count = held.count;
            let permitted = held
                .controller
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .permitted();
            permitted.saturating_sub(count)
        };
        permitted.min(self.ceilings.global.get().saturating_sub(state.in_flight))
    }
}
