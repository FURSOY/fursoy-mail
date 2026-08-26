## Any mailbox, not only Gmail

- Accounts now connect over IMAP and SMTP, so any provider can be added: Gmail and Outlook sign in through the browser, and everything else uses an app password or manual server settings.
- Folders are discovered from the server — Inbox, Sent, Archive, Spam, Trash and your own folders — and new mail arrives over a live connection rather than on a timer.
- Folder and label names written in Turkish or any other non-ASCII alphabet are readable at last, instead of appearing as `&AMcA9g-p kutusu`.

## Labels

- Labels are read back from the server, so one applied on your phone or in the web interface appears here, and a message keeps its labels when it is moved.
- Labels can be applied to several conversations at once from the selection toolbar, and opened from any row in the list.
- A label made or deleted in another client appears and disappears here on its own; renaming one carries its nested labels with it.
- Names that are mailboxes on Gmail — Trash, Inbox, Spam — are refused: applying one of those as a label moved the message instead of tagging it.
- The label list is offered only for accounts whose server can hold a tag.

## Folders

- Conversations can be moved into your own folders, from the reader or from the selection toolbar.
- Folders can be created and renamed from the sidebar, and the folder list collapses like the label list.
- Advanced search can be scoped to one of your folders.
- A folder deleted on the server no longer leaves its messages behind in the local cache.

## New mail

- A notification raised while a full-screen application is running is held until the screen is free, instead of being dropped and never shown.
- One summary notification stands in for what was missed: mail that arrived while the application was closed, during quiet hours, or a burst longer than five messages.
- Mail read, starred or labelled somewhere else shows up without waiting for the hourly check.
- The message list keeps refreshing after it has been paged through, and an open conversation shows a reply as it arrives.
- Opening a mail from a notification marks it read, and a notification whose subject another one shares now opens the newer message.
- The refresh button shows that it is working, and mail is fetched at once when the window is brought forward or the machine comes back online.
- The tray icon's tooltip carries the unread count.

## Sign-in and sessions

- Microsoft sign-in works: a session too large for the Windows credential store was rejected after the mailbox had already signed in, and the account was never created.
- A sign-in waiting on the browser can be retried without closing the window, and a failed one says why instead of showing a general message.
- Only a session the provider itself rejected asks for a new sign-in; a network failure no longer looks like an expired account. Sessions are renewed on a timer before anything is waiting on them.

## Appearance

- The scrollbars in the label and folder lists are visible, and sit on the edge of the sidebar.
