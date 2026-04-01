use anyhow::Result;
use k256::ecdsa::{SigningKey, Signature, signature::Signer};
use serde_json::{json, Value};
use sha3::{Digest, Keccak256};

/// EIP-712 signer for Hyperliquid exchange actions.
/// Port of HyperliquidSigner from t-bot (Java).
pub struct HyperliquidSigner {
    signing_key: SigningKey,
    wallet_address: String,
}

impl HyperliquidSigner {
    pub fn new(private_key_hex: &str, wallet_address: String) -> Result<Self> {
        let key_bytes = hex::decode(private_key_hex.trim_start_matches("0x"))?;
        if key_bytes.len() != 32 {
            return Err(anyhow::anyhow!(
                "Invalid private key length: expected 32 bytes, got {}",
                key_bytes.len()
            ));
        }
        let signing_key = SigningKey::from_bytes((&key_bytes[..]).into())
            .map_err(|e| anyhow::anyhow!("Invalid private key: {}", e))?;
        Ok(Self {
            signing_key,
            wallet_address,
        })
    }

    pub fn wallet_address(&self) -> &str {
        &self.wallet_address
    }

    /// Sign an action for the /exchange endpoint.
    /// Returns the signature components (r, s, v) as a JSON-compatible object.
    pub fn sign_action(
        &self,
        action: &Value,
        vault_address: Option<&str>,
        nonce: u64,
    ) -> Result<Value> {
        let connection_id = self.action_hash(action, vault_address, nonce)?;

        let signature: Signature = self.signing_key.sign(&connection_id);
        let sig_bytes = signature.to_bytes();

        Ok(json!({
            "r": format!("0x{}", hex::encode(&sig_bytes[..32])),
            "s": format!("0x{}", hex::encode(&sig_bytes[32..64])),
            "v": 27
        }))
    }

    /// Compute the EIP-712 typed data hash for a Hyperliquid action.
    fn action_hash(
        &self,
        action: &Value,
        vault_address: Option<&str>,
        nonce: u64,
    ) -> Result<Vec<u8>> {
        // Hyperliquid uses a specific EIP-712 domain
        let domain_separator = self.domain_separator();

        // Encode the action + nonce + vault into a struct hash
        let action_str = serde_json::to_string(action)?;
        let mut message_data = Vec::new();

        // TypeHash for HyperliquidTransaction:Agent
        let type_hash = Keccak256::digest(
            b"HyperliquidTransaction:Agent(string source,string connectionId)"
        );
        message_data.extend_from_slice(&type_hash);

        // source = "a" (API)
        let source_hash = Keccak256::digest(b"a");
        message_data.extend_from_slice(&source_hash);

        // connectionId = keccak256(action_json + nonce + vault)
        let mut connection_parts = action_str.as_bytes().to_vec();
        connection_parts.extend_from_slice(&nonce.to_be_bytes());
        if let Some(vault) = vault_address {
            connection_parts.extend_from_slice(vault.as_bytes());
        }
        let connection_hash = Keccak256::digest(&connection_parts);
        message_data.extend_from_slice(&connection_hash);

        let struct_hash = Keccak256::digest(&message_data);

        // Final hash = keccak256(0x1901 + domainSeparator + structHash)
        let mut final_data = vec![0x19, 0x01];
        final_data.extend_from_slice(&domain_separator);
        final_data.extend_from_slice(&struct_hash);

        Ok(Keccak256::digest(&final_data).to_vec())
    }

    /// EIP-712 domain separator for Hyperliquid.
    fn domain_separator(&self) -> Vec<u8> {
        let type_hash = Keccak256::digest(
            b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)"
        );
        let name_hash = Keccak256::digest(b"Exchange");
        let version_hash = Keccak256::digest(b"1");

        // chainId = 1337 for mainnet Hyperliquid
        let chain_id: [u8; 32] = {
            let mut buf = [0u8; 32];
            buf[31] = 0x39; // 1337 & 0xFF
            buf[30] = 0x05; // (1337 >> 8) & 0xFF
            buf
        };

        let verifying_contract = [0u8; 32]; // zero address

        let mut data = Vec::new();
        data.extend_from_slice(&type_hash);
        data.extend_from_slice(&name_hash);
        data.extend_from_slice(&version_hash);
        data.extend_from_slice(&chain_id);
        data.extend_from_slice(&verifying_contract);

        Keccak256::digest(&data).to_vec()
    }

    /// Generate a nonce (current timestamp in millis).
    pub fn generate_nonce() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }

    /// Build a signed request payload for the /exchange endpoint.
    pub fn build_signed_request(
        &self,
        action: Value,
        vault_address: Option<&str>,
    ) -> Result<Value> {
        let nonce = Self::generate_nonce();
        let signature = self.sign_action(&action, vault_address, nonce)?;

        Ok(json!({
            "action": action,
            "nonce": nonce,
            "signature": signature,
            "vaultAddress": vault_address,
        }))
    }
}
