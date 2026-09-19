# WEBSITE_RULES — FURSOY product website

This directory contains the public web presence of this product only
(product site, download, privacy/terms and other public pages).

## Canonical design standard

All FURSOY websites follow the canonical FURSOY Web Design Standard:

    C:/Projeler/fursoy-vault/website/FURSOY_WEB_GUIDELINES.md

Read it before making visual or structural changes. It defines the
canonical (MUST) rules and the adaptable (REFERENCE) values. This file
is version-controlled with the reference implementation.

## Core FURSOY identity (fallback if the guideline is unavailable)

- Manrope is the primary typeface; Space Mono is used for small
  labels/kickers only.
- Use the FURSOY ink / teal / bright / paper color family
  (`--ink #0b3334`, `--teal #13abb8`, `--bright #27d2d8`,
  `--paper #f6f8f5`); dark surfaces use shades of the ink family, never
  gray/black.
- Content containers follow the ~1180px FURSOY layout system
  (`min(1180px, 100% - 40px)`; narrow content 760–850px).
- Large surfaces/cards use the larger radius family (17–26px); controls
  use the smaller one (7–13px); pills are fully rounded (999px).
- Borders are thin (1px) and restrained.
- Headings are tightly tracked; body text is small and light with
  generous line-height; small mono uppercase labels are part of the
  identity.
- Buttons are bold (13px/800), solid-ink primary + bordered secondary,
  with a ~200ms hover lift; motion overall is minimal and functional;
  respect `prefers-reduced-motion`.
- Navbar: bordered bar, brand left, pill CTA right. Footer: dark ink
  surface with small mono column headers.
- Buttons, labels, spacing and geometry must remain recognizably
  FURSOY. Do not invent a new visual language.

## Rules

- Preserve the core identity above; adapt adaptable REFERENCE values to
  the product's content. Page composition may differ, but the site must
  visually belong to the FURSOY product family. This site presents
  dark-first; keep backgrounds in the ink family and accents in the
  teal/bright family.
- Do not add analytics, tracking or telemetry without an explicit
  product decision. (This is a product/privacy convention, not part of
  the design system.)
- Keep canonical URLs, robots.txt, sitemap and OG tags pointing at this
  site's own canonical domain; change them together when the domain
  changes.

## Directory scope

`/website` contains the product's public web presence.

Application runtime, backend or product source code must not be moved
into this directory unless it is specifically part of the website
runtime. Keep the website's own build and deploy configuration inside
this directory. This site is dependency-free static HTML/CSS served by
Cloudflare Workers assets; keep it that way.
