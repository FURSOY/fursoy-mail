# Google OAuth release checklist

This file tracks the external steps required before FURSOY Mail's Google OAuth app is made broadly available. The English privacy policy is maintained in `PRIVACY.md`.

## Repository work

- [x] Request one Gmail scope that matches the current feature set: `gmail.modify`.
- [x] Remove the redundant `gmail.send` scope.
- [x] Explain local storage, Google access, remote images, and update traffic in-product.
- [x] Publish an English privacy policy in the repository.
- [ ] Host the product homepage and privacy policy on a domain controlled by the developer.
- [ ] Replace the in-app repository policy link with the final policy URL.

## Google Cloud Console

1. Add and verify the product domain in Google Search Console.
2. Configure the OAuth consent screen with the exact public product name, support contact, logo, homepage, and privacy-policy URL.
3. Add only these scopes:
   - `https://www.googleapis.com/auth/gmail.modify`
   - `https://www.googleapis.com/auth/userinfo.profile`
   - `https://www.googleapis.com/auth/userinfo.email`
4. Make sure the homepage clearly links to the privacy policy and explains the app's Gmail-related features.
5. Prepare an unlisted verification video in English. Show the complete authorization flow, the consent screen and requested scopes, then demonstrate the features that use them: sync/read, notifications and OTP detection, sending, archive/trash/label actions, and account removal.
6. Add test users while the consent screen remains in testing.
7. Submit the OAuth app for restricted-scope verification. FURSOY Mail processes and stores Gmail data on the user's device instead of a developer-controlled third-party server; document this clearly in the submission. Google's current guidance ties the additional security assessment to restricted data accessed from, transmitted through, or stored on a third-party server, but Google makes the final determination during review.

## Information still needed

- A developer-controlled public domain
- A public support/privacy email address
- Final homepage URL
- Final privacy-policy URL on the same verified domain
