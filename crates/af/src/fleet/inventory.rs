use std::collections::BTreeMap;

use af_contracts::fleet::{Allocation, Capacity, Host, Inventory, ReleasePolicy};
use serde::Deserialize;

use super::transport::ReadFailure;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Pools {
    // Resource pool membership lists contain VM names.
    #[serde(rename = "static")]
    _static_members: Vec<String>,
    #[serde(rename = "dynamic")]
    _dynamic_members: Vec<String>,
    release: ReleasePolicy,
    available: Vec<Available>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Available {
    node: String,
    cpu_ids: Vec<u32>,
    ram_gib: f64,
    ssd_gib: f64,
    static_available: Option<Capacity>,
}

fn capacity(mut capacity: Capacity) -> Result<Capacity, ReadFailure> {
    capacity.cpu_ids.sort_unstable();
    capacity.cpu_ids.dedup();
    if capacity.ram_gib >= 0.0 && capacity.ssd_gib >= 0.0 {
        Ok(capacity)
    } else {
        Err(ReadFailure::InvalidResponse)
    }
}

impl Pools {
    pub(super) fn normalize(self) -> Result<Inventory, ReadFailure> {
        let mut hosts = BTreeMap::new();
        for host in self.available {
            let available = capacity(Capacity {
                cpu_ids: host.cpu_ids,
                ram_gib: host.ram_gib,
                ssd_gib: host.ssd_gib,
            })?;
            let static_available = host.static_available.map(capacity).transpose()?;
            if host.node.is_empty() || hosts.contains_key(&host.node) {
                return Err(ReadFailure::InvalidResponse);
            }
            hosts.insert(
                host.node.clone(),
                Host {
                    node: host.node,
                    available,
                    static_available,
                    physical_totals: None,
                    reservations: None,
                },
            );
        }
        Ok(Inventory {
            hosts: hosts.into_values().collect(),
            pools: None,
            release: self.release,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BrokerAllocation {
    vmid: u64,
    name: Option<String>,
    node: Option<String>,
    power: Option<String>,
    state: Option<String>,
    charged: Option<Capacity>,
    #[serde(rename = "task")]
    _task: Option<String>,
}

pub(super) fn allocations(rows: Vec<BrokerAllocation>) -> Result<Vec<Allocation>, ReadFailure> {
    let mut allocations = BTreeMap::new();
    for row in rows {
        if allocations.contains_key(&row.vmid) || row.vmid == 0 {
            return Err(ReadFailure::InvalidResponse);
        }
        allocations.insert(
            row.vmid,
            Allocation {
                vmid: row.vmid,
                name: row.name,
                node: row.node,
                power: row.power,
                state: row.state,
                charged: row.charged.map(capacity).transpose()?,
                executor: None,
            },
        );
    }
    Ok(allocations.into_values().collect())
}
