## Threaded conversations and drafts

- Reply, Reply All, and Forward now operate on the selected message inside a conversation.
- Added restorable inline Gmail drafts with correct recipients and RFC reply headers.
- Improved draft reconciliation after sending or deleting a reply.
- Added thread-aware archive, trash, unread, and spam actions.

## Search and synchronization

- Added fast full-message search backed by SQLite FTS5, including in-message highlighting.
- Virtualized long message lists and optimized incremental search to keep the interface responsive.
- Added resumable background synchronization for mailboxes larger than the initial sync window.

## Message reader

- Improved HTML and plain-text rendering, spacing, fit behavior, and message details.
- Refined desktop and mobile layouts for multi-message conversations.

## Reliability, security, and maintenance

- Kept desktop OAuth protected with PKCE and compatible with the configured Google client credentials.
- Updated frontend and Rust dependencies, license notices, privacy terms, and security guidance.
- Added regression coverage for OAuth, search, sync, threads, recipients, replies, drafts, and rendering.
