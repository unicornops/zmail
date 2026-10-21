// Ported from go-proton-api `event.go` (`GetLatestEventID`) at
// 8a62023d3e38cb29749bcfc01bf44eafda6cec82
// (https://github.com/ProtonMail/go-proton-api).
// Copyright (c) 2020 James Houlahan, Copyright (c) 2022 Proton AG.
// Licensed under the MIT License; see crates/zmail-proton-api/LICENSE-MIT.
// Modified by UnicornOps on 2026-10-21: rewritten in Rust. The event
// stream and event types follow with the event loop (#237).

//! The account's event stream.

use serde::Deserialize;

use crate::{client::Client, manager::Request, response::Error};

impl Client {
    /// Returns the ID of the newest event (`GetLatestEventID`); syncing from
    /// it later replays everything that happened after this call.
    pub async fn get_latest_event_id(&self) -> Result<String, Error> {
        #[derive(Deserialize)]
        struct Res {
            #[serde(rename = "EventID")]
            event_id: String,
        }
        let res: Res = self.json(&Request::get("/core/v4/events/latest")).await?;
        Ok(res.event_id)
    }
}
