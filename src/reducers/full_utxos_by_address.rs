use pallas::codec::utils::CborWrap;
use pallas::crypto::hash::Hash;
use pallas::ledger::primitives::babbage::{DatumOption, PlutusData};
use pallas::ledger::primitives::Fragment;
use pallas::ledger::traverse::{Asset, MultiEraBlock, MultiEraTx};
use pallas::ledger::traverse::{MultiEraOutput, OriginalHash};
use serde::Deserialize;
use serde_json::json;

use crate::{crosscut, model, prelude::*};

#[derive(Deserialize)]
pub struct Config {
    pub filter: Vec<String>,
    pub prefix: Option<String>,
    pub address_as_key: Option<bool>,
}

pub struct Reducer {
    config: Config,
    policy: crosscut::policies::RuntimePolicy,
}

pub fn resolve_datum(utxo: &MultiEraOutput, tx: &MultiEraTx) -> Result<PlutusData, ()> {
    match utxo.datum() {
        Some(DatumOption::Data(CborWrap(pd))) => Ok(pd),
        Some(DatumOption::Hash(datum_hash)) => {
            for raw_datum in tx.clone().plutus_data() {
                if raw_datum.original_hash().eq(&datum_hash) {
                    return Ok(raw_datum.clone().unwrap());
                }
            }

            return Err(());
        }
        _ => Err(()),
    }
}

impl Reducer {
    fn get_key_value(
        &self,
        utxo: &MultiEraOutput,
        tx: &MultiEraTx,
        output_ref: &(Hash<32>, u64),
    ) -> Option<(String, String)> {
        if let Some(address) = utxo.address().map(|addr| addr.to_string()).ok() {
            if self.config.filter.iter().any(|addr| address.eq(addr)) {
                let mut data = serde_json::Value::Object(serde_json::Map::new());
                let address_as_key = self.config.address_as_key.unwrap_or(false);
                let key: String;

                if address_as_key {
                    key = address;
                    data["tx_hash"] = serde_json::Value::String(hex::encode(output_ref.0.to_vec()));
                    data["output_index"] =
                        serde_json::Value::from(serde_json::Number::from(output_ref.1));
                } else {
                    key = format!("{}#{}", hex::encode(output_ref.0.to_vec()), output_ref.1);
                    data["address"] = serde_json::Value::String(address);
                }

                if let Some(datum) = resolve_datum(utxo, tx).ok() {
                    data["datum"] = serde_json::Value::String(hex::encode(
                        datum.encode_fragment().ok().unwrap(),
                    ));
                } else if let Some(DatumOption::Hash(h)) = utxo.datum() {
                    data["datum_hash"] = serde_json::Value::String(hex::encode(h.to_vec()));
                }

                let mut assets: Vec<serde_json::Value> = Vec::new();
                for asset in utxo.non_ada_assets() {
                    match asset {
                        Asset::Ada(lovelace_amt) => {
                            assets.push(json!({
                                "unit": "lovelace",
                                "quantity": format!("{}", lovelace_amt)
                            }));
                        }
                        Asset::NativeAsset(cs, tkn, amt) => {
                            let unit = format!("{}{}", hex::encode(cs.to_vec()), hex::encode(tkn));
                            assets.push(json!({
                                "unit": unit,
                                "quantity": format!("{}", amt)
                            }));
                        }
                    }
                }

                data["amount"] = serde_json::Value::Array(assets);
                return Some((key, data.to_string()));
            }
        }

        None
    }

    pub fn reduce_block<'b>(
        &mut self,
        block: &'b MultiEraBlock<'b>,
        ctx: &model::BlockContext,
        output: &mut super::OutputPort,
    ) -> Result<(), gasket::error::Error> {
        let prefix = self.config.prefix.as_deref();
        for tx in block.txs().into_iter() {
            for consumed in tx.consumes().iter().map(|i| i.output_ref()) {
                if let Some(Some(utxo)) = ctx.find_utxo(&consumed).apply_policy(&self.policy).ok() {
                    if let Some((key, value)) =
                        self.get_key_value(&utxo, &tx, &(consumed.hash().clone(), consumed.index()))
                    {
                        output.send(
                            model::CRDTCommand::set_remove(prefix, &key.as_str(), value).into(),
                        )?;
                    }
                }
            }

            for (index, produced) in tx.produces() {
                let output_ref = (tx.hash().clone(), index as u64);
                if let Some((key, value)) = self.get_key_value(&produced, &tx, &output_ref) {
                    output.send(model::CRDTCommand::set_add(None, &key, value).into())?;
                }
            }
        }

        Ok(())
    }
}

impl Config {
    pub fn plugin(self, policy: &crosscut::policies::RuntimePolicy) -> super::Reducer {
        let reducer = Reducer {
            config: self,
            policy: policy.clone(),
        };

        super::Reducer::FullUtxosByAddress(reducer)
    }
}
