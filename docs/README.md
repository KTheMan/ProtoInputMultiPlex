# Documentation

[Project home](../README.md) · [Download the RC](https://github.com/KTheMan/ProtoInputMultiPlex/releases/tag/v0.1.0-rc.1)

## Start here

| Task | Guide |
| --- | --- |
| Launch the portable EXE or install the application | [Installation, upgrades and recovery](installation-recovery.md) |
| Try two virtual users with built-in test windows | [First session](first-session.md) |
| Choose routing modes and target processes | [Desktop routing](concurrent-routing.md) |
| Understand compatibility and report a problem | [Known limits](known-limitations.md) |
| Configure an MCP client for the released RC | [Pinned RC agent guide](https://github.com/KTheMan/ProtoInputMultiPlex/blob/d38f5676b81568f024d7ea989b883cf9f407f04a/docs/agents.md) |

## Input and automation

These references describe feature behavior and its recorded validation scope.

| Area | References |
| --- | --- |
| Users, devices and targets | [Explicit window selection](explicit-window-selection-contract.md), [keyboard delivery](keyboard-delivery-contract.md), [keyboard state](keyboard-configuration-contract.md) |
| Motion, buttons and wheel | [Input conversion](input-conversion-plan.md), [horizontal wheel](horizontal-wheel-plan.md), [conditional wheel](conditional-wheel-plan.md) |
| Cursors and bounds | [Cursor appearance](cursor-appearance-next-plan.md), [window lock](window-lock-workflow.md), [GPU cursor presentation](gpu-cursor-presentation.md) |
| Macros and recording | [Macro Studio](connected-macro-studio-plan.md), [recording](connected-macro-recording-contract.md), [macro files](gui-macro-file-contract.md) |
| Replication and virtual replicas | [Graph control](connected-graph-contract.md), [advanced replication](advanced-replication-plan.md), [virtual replicas](connected-virtual-users-contract.md) |
| Mapper and screen observations | [Mapper](input-mapper-contract.md), [screen tools](screen-tools.md), [Mapper screen nodes](mapper-screen.md), [text output](mapper-type-text-contract.md), [OCR](ocr-text-contract.md) |
| Drawing and magnification | [Drawing controls](draw-access-keys-contract.md), [annotation export](annotation-export-workflow-plan.md), [magnifier controls](magnifier-controls-contract.md) |
| Profiles and application preferences | [Installation and data recovery](installation-recovery.md), [profile maintenance](profile-maintenance-plan.md), [persistence](profile-persistence-contract.md) |
| Stop and recovery | [Emergency Stop](shared-emergency-stop-contract.md), [clean restart](clean-restart-contract.md), [failed cleanup recovery](failed-cleanup-recovery-contract.md) |

## Agents and SDKs

| Task | Reference |
| --- | --- |
| Use standalone or GUI-connected MCP with the RC | [Pinned RC agent guide](https://github.com/KTheMan/ProtoInputMultiPlex/blob/d38f5676b81568f024d7ea989b883cf9f407f04a/docs/agents.md) |
| Read the source tree's MCP reference | [Agent guide](agents.md) |
| Set permissions for a GUI connection | [GUI agent contract](gui-agent-plan.md) |
| Build a cooperative native/web integration | [Application-event SDK](https://github.com/KTheMan/ProtoInputMultiPlex/blob/d38f5676b81568f024d7ea989b883cf9f407f04a/docs/application-event-sdk.md) |
| Understand the loopback SDK | [Loopback SDK](loopback-sdk.md) |
| Read the example client's setup and contracts | [Rust MCP client](../examples/rust-mcp-client/README.md) |

## Development and evidence

| Topic | Reference |
| --- | --- |
| Build, test and contribute | [Development guide](development.md) |
| Native integration and compatibility patches | [Backend](backend.md), [Rust port proposal](protoinput-rust-port-proposal.md) |
| Packaging and Windows metadata | [Portable packaging](portable-packaging.md), [version metadata](windows-version-metadata.md) |
| Current RC acceptance | [Pinned VM105 validation record](https://github.com/KTheMan/ProtoInputMultiPlex/blob/d38f5676b81568f024d7ea989b883cf9f407f04a/docs/validation.md), [public qualification summary](https://github.com/KTheMan/ProtoInputMultiPlex/releases/download/v0.1.0-rc.1/qualification.json) |
| Earlier feature assessments | [Validation history](validation.md) |
| Product scope and remaining work | [Ultra parity](ultra-parity.md), [feature planning baseline](feature-catalog.md), [roadmap](roadmap.md) |
| Detailed notes moved from the front page | [Preserved README history](history/readme-2026-11-17.md) |

## Policies

[Clean-room implementation](development.md#clean-room-implementation) · [Project license](../LICENSE) · [Third-party notices](../THIRD_PARTY_NOTICES.md) · [Privacy](privacy.md) · [Code signing](code-signing-policy.md)
