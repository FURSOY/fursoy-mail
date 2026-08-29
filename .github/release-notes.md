## Sending and drafts

- Sending a saved draft now stamps a From header on it; without one, receiving servers were free to reject the message outright, and any other IMAP client opening the same Drafts folder showed it with no sender.
- Outbound mail now carries a Date header. A draft written straight to the mailbox got no date from anywhere else and showed none in other clients, sorting as 1970.
- Editing or deleting a draft on a server without UIDPLUS no longer fails outright: it used to leave the previous copy of an edited draft stranded in the mailbox and make the delete button do nothing.
