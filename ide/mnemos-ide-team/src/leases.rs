// SPDX-License-Identifier: AGPL-3.0-only
//! Leases on resources several sessions share: `cpu` (one heavy build or
//! test run at a time), `chrome` (the one browser) and `port` (a free port
//! number each). A lease ends when it is released, when its time runs out, or
//! when its holder's pane closes, so a dead session never blocks the others.

use std::time::Duration;

use instant::Instant;
use serde::Serialize;

pub const MIN_TTL: Duration = Duration::from_secs(10);
pub const MAX_TTL: Duration = Duration::from_secs(2 * 60 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Resource {
    Cpu,
    Chrome,
    Port,
}

impl Resource {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "cpu" => Ok(Self::Cpu),
            "chrome" => Ok(Self::Chrome),
            "port" => Ok(Self::Port),
            other => Err(format!("unknown resource {other}; use cpu, chrome or port")),
        }
    }

    /// Whether only one holder at a time may have it.
    fn exclusive(self) -> bool {
        !matches!(self, Self::Port)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Lease {
    pub id: String,
    pub resource: Resource,
    /// The holder's terminal (an opaque number from the app).
    #[serde(skip)]
    pub holder: u64,
    pub holder_name: String,
    pub port: Option<u16>,
    #[serde(skip)]
    pub expires: Instant,
}

#[derive(Debug, Default)]
pub struct LeaseTable {
    leases: Vec<Lease>,
    next: u64,
}

impl LeaseTable {
    /// Drops expired leases and those whose holder is gone.
    pub fn purge(&mut self, now: Instant, alive: impl Fn(u64) -> bool) {
        self.leases.retain(|l| l.expires > now && alive(l.holder));
    }

    /// Takes (or, for the same holder, renews) a lease. `free_port` finds a
    /// port for a `port` lease; ports another lease holds are never returned.
    pub fn acquire(
        &mut self,
        resource: Resource,
        holder: u64,
        holder_name: &str,
        ttl: Duration,
        now: Instant,
        free_port: impl FnOnce(&[u16]) -> Option<u16>,
    ) -> Result<Lease, String> {
        let ttl = ttl.clamp(MIN_TTL, MAX_TTL);
        if resource.exclusive() {
            if let Some(held) = self.leases.iter_mut().find(|l| l.resource == resource) {
                if held.holder != holder {
                    let left = held.expires.saturating_duration_since(now).as_secs();
                    return Err(format!(
                        "{} is leased by {} for up to {left} s more; retry later",
                        name(resource),
                        held.holder_name
                    ));
                }
                held.expires = now + ttl;
                return Ok(held.clone());
            }
        }
        let port = if resource == Resource::Port {
            let taken: Vec<u16> = self.leases.iter().filter_map(|l| l.port).collect();
            Some(free_port(&taken).ok_or("no free port found")?)
        } else {
            None
        };
        self.next += 1;
        let lease = Lease {
            id: format!("{}-{}", name(resource), self.next),
            resource,
            holder,
            holder_name: holder_name.to_string(),
            port,
            expires: now + ttl,
        };
        self.leases.push(lease.clone());
        Ok(lease)
    }

    /// Releases lease `id` if `holder` holds it.
    pub fn release(&mut self, id: &str, holder: u64) -> Result<(), String> {
        let before = self.leases.len();
        self.leases.retain(|l| !(l.id == id && l.holder == holder));
        if self.leases.len() == before {
            Err(format!("you hold no lease {id}"))
        } else {
            Ok(())
        }
    }

    /// The current leases with their remaining seconds.
    pub fn list(&self, now: Instant) -> Vec<(Lease, u64)> {
        self.leases
            .iter()
            .map(|l| (l.clone(), l.expires.saturating_duration_since(now).as_secs()))
            .collect()
    }
}

fn name(resource: Resource) -> &'static str {
    match resource {
        Resource::Cpu => "cpu",
        Resource::Chrome => "chrome",
        Resource::Port => "port",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: Duration = Duration::from_secs(60);

    #[test]
    fn exclusive_resources_have_one_holder() {
        let mut t = LeaseTable::default();
        let now = Instant::now();
        let a = t.acquire(Resource::Cpu, 1, "builder", MIN, now, |_| None).unwrap();
        let err = t.acquire(Resource::Cpu, 2, "tester", MIN, now, |_| None).unwrap_err();
        assert!(err.contains("leased by builder"), "{err}");
        // The holder renews instead of stacking.
        let renewed = t.acquire(Resource::Cpu, 1, "builder", MIN * 2, now, |_| None).unwrap();
        assert_eq!(renewed.id, a.id);
        // Another resource is independent.
        t.acquire(Resource::Chrome, 2, "tester", MIN, now, |_| None).unwrap();
    }

    #[test]
    fn leases_end_on_release_expiry_and_dead_holders() {
        let mut t = LeaseTable::default();
        let now = Instant::now();
        let a = t.acquire(Resource::Cpu, 1, "a", MIN, now, |_| None).unwrap();
        assert!(t.release(&a.id, 2).is_err(), "only the holder releases");
        t.release(&a.id, 1).unwrap();
        t.acquire(Resource::Cpu, 2, "b", MIN, now, |_| None).unwrap();

        t.purge(now + MIN + Duration::from_secs(1), |_| true);
        t.acquire(Resource::Cpu, 3, "c", MIN, now + MIN * 2, |_| None).unwrap();
        t.purge(now + MIN * 2, |holder| holder != 3);
        assert!(t.list(now).is_empty());
    }

    #[test]
    fn ports_are_distinct() {
        let mut t = LeaseTable::default();
        let now = Instant::now();
        let p1 = t.acquire(Resource::Port, 1, "a", MIN, now, |_| Some(5001)).unwrap();
        let p2 = t
            .acquire(Resource::Port, 1, "a", MIN, now, |taken| {
                assert_eq!(taken, &[5001]);
                Some(5002)
            })
            .unwrap();
        assert_ne!(p1.id, p2.id);
        assert_eq!(p2.port, Some(5002));
    }

    #[test]
    fn ttl_is_clamped() {
        let mut t = LeaseTable::default();
        let now = Instant::now();
        let l = t.acquire(Resource::Chrome, 1, "a", Duration::from_secs(1), now, |_| None).unwrap();
        assert_eq!(l.expires, now + MIN_TTL);
        assert!(Resource::parse("gpu").is_err());
    }
}
