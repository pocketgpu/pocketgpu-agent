//! The guest page's way to the computer (F48): iroh in the browser, always through our
//! relays (browsers can't make direct links), end to end encrypted to the computer's key.
//!
//! JavaScript gets one class, `Guest`: connect with the link's fragment and this browser's
//! key, claim the share once, ask its status, and chat. Replies are the protocol's own
//! messages as plain objects (`{ Claimed: {...} }`, `{ Refused: "Ended" }`, …).

#[cfg(target_family = "wasm")]
use getrandom as _;

#[cfg(target_family = "wasm")]
mod web {
    use iroh::endpoint::{Connection, presets};
    use iroh::{Endpoint, EndpointAddr, PublicKey, RelayMode, RelayUrl, SecretKey};
    use pocketgpu_proto::pairing::proof;
    use pocketgpu_proto::share::{AgentToGuest, ChatMsg, GuestToAgent, ShareLink};
    use pocketgpu_proto::{alpn, read_frame, write_frame};
    use wasm_bindgen::prelude::*;

    fn err(e: impl std::fmt::Display) -> JsError {
        JsError::new(&e.to_string())
    }

    #[wasm_bindgen(start)]
    fn start() {
        console_error_panic_hook::set_once();
    }

    /// A new key for this browser, to keep with the link.
    #[wasm_bindgen(js_name = newSecretKey)]
    pub fn new_secret_key() -> Vec<u8> {
        SecretKey::generate().to_bytes().to_vec()
    }

    #[wasm_bindgen]
    pub struct Guest {
        endpoint: Endpoint,
        conn: Connection,
        link: ShareLink,
        key: SecretKey,
    }

    #[wasm_bindgen]
    impl Guest {
        /// Connects to the link's computer through each of its relays in turn.
        pub async fn connect(fragment: String, secret: Vec<u8>) -> Result<Guest, JsError> {
            let link = ShareLink::from_fragment(&fragment).map_err(err)?;
            let bytes: [u8; 32] = secret
                .try_into()
                .map_err(|_| JsError::new("this browser's key is damaged"))?;
            let key = SecretKey::from_bytes(&bytes);
            let relays: Vec<RelayUrl> = link.relays.iter().filter_map(|r| r.parse().ok()).collect();
            let endpoint = Endpoint::builder(presets::Minimal)
                .relay_mode(RelayMode::custom(relays.clone()))
                .secret_key(key.clone())
                .bind()
                .await
                .map_err(err)?;
            let computer = PublicKey::from_bytes(&link.computer).map_err(err)?;
            let mut last = None;
            for relay in relays {
                let addr = EndpointAddr::new(computer).with_relay_url(relay);
                match endpoint.connect(addr, alpn::MODEL_GUEST).await {
                    Ok(conn) => {
                        return Ok(Guest {
                            endpoint,
                            conn,
                            link,
                            key,
                        });
                    }
                    Err(e) => last = Some(e.to_string()),
                }
            }
            Err(JsError::new(&format!(
                "unreachable: {}",
                last.unwrap_or_else(|| "no relay".into())
            )))
        }

        /// Claims the share for this browser; the computer answers `Claimed` or `Refused`.
        pub async fn claim(&self) -> Result<JsValue, JsError> {
            let req = GuestToAgent::Claim {
                share: self.link.share,
                proof: proof(
                    &self.link.secret,
                    &self.link.computer,
                    self.key.public().as_bytes(),
                ),
            };
            let replies = self.ask(&req, None).await?;
            to_js(replies.into_iter().next())
        }

        /// What the share is, without claiming it (the welcome page).
        pub async fn peek(&self) -> Result<JsValue, JsError> {
            let req = GuestToAgent::Peek {
                share: self.link.share,
                proof: proof(
                    &self.link.secret,
                    &self.link.computer,
                    self.key.public().as_bytes(),
                ),
            };
            let replies = self.ask(&req, None).await?;
            to_js(replies.into_iter().next())
        }

        pub async fn status(&self) -> Result<JsValue, JsError> {
            let replies = self.ask(&GuestToAgent::Status, None).await?;
            to_js(replies.into_iter().next())
        }

        /// Sends the chat (an array of `{ role: "User" | "Assistant", text }`); calls
        /// `on_text` with each part of the answer; resolves with `Done` or `Refused`. A
        /// `Speed` the computer sent comes inside `Done`, as `{ used_today, speed }`.
        pub async fn chat(
            &self,
            messages: JsValue,
            on_text: js_sys::Function,
        ) -> Result<JsValue, JsError> {
            let messages: Vec<ChatMsg> = serde_wasm_bindgen::from_value(messages).map_err(err)?;
            let replies = self
                .ask(&GuestToAgent::Chat { messages }, Some(&on_text))
                .await?;
            let speed = replies.iter().find_map(|r| match r {
                AgentToGuest::Speed { tokens_per_s, ms } => Some((*tokens_per_s, *ms)),
                _ => None,
            });
            let last = replies
                .into_iter()
                .rfind(|r| !matches!(r, AgentToGuest::Speed { .. }));
            let reply = to_js(last)?;
            if let Some((tokens_per_s, ms)) = speed
                && let Ok(done) = js_sys::Reflect::get(&reply, &JsValue::from_str("Done"))
                && done.is_object()
            {
                let s = js_sys::Object::new();
                let set = |k: &str, v: u32| js_sys::Reflect::set(&s, &k.into(), &v.into());
                let _ = set("tokens_per_s", tokens_per_s);
                let _ = set("ms", ms);
                let _ = js_sys::Reflect::set(&done, &"speed".into(), &s);
            }
            Ok(reply)
        }

        /// Closes the connection; the computer keeps this browser's place.
        pub async fn close(self) {
            self.conn.close(0u32.into(), b"bye");
            self.endpoint.close().await;
        }
    }

    impl Guest {
        /// One request on its own stream; `Text` parts go to `on_text`, the rest are returned.
        async fn ask(
            &self,
            req: &GuestToAgent,
            on_text: Option<&js_sys::Function>,
        ) -> Result<Vec<AgentToGuest>, JsError> {
            let (mut send, mut recv) = self.conn.open_bi().await.map_err(err)?;
            write_frame(&mut send, req).await.map_err(err)?;
            send.finish().map_err(err)?;
            let mut out = Vec::new();
            loop {
                match read_frame::<_, AgentToGuest>(&mut recv).await {
                    Ok(Some(AgentToGuest::Text(t))) => {
                        if let Some(f) = on_text {
                            let _ = f.call1(&JsValue::NULL, &JsValue::from_str(&t));
                        }
                    }
                    Ok(Some(m)) => out.push(m),
                    Ok(None) => break,
                    Err(e) => return Err(err(e)),
                }
            }
            Ok(out)
        }
    }

    fn to_js(reply: Option<AgentToGuest>) -> Result<JsValue, JsError> {
        let reply = reply.ok_or_else(|| JsError::new("the computer closed without an answer"))?;
        serde_wasm_bindgen::to_value(&reply).map_err(err)
    }
}
