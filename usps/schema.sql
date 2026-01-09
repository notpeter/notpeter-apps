-- USPS Stamps Database Schema
-- This file is the source of truth for the database schema.

-- stampsforever_stamps: Raw data from stampsforever.com API listing
CREATE TABLE IF NOT EXISTS stampsforever_stamps (
    slug TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    url TEXT NOT NULL,
    rate TEXT,
    year INTEGER,
    issue_date TEXT,
    issue_location TEXT,
    type TEXT NOT NULL DEFAULT 'stamp'
);

CREATE INDEX IF NOT EXISTS idx_stampsforever_stamps_year ON stampsforever_stamps(year);

-- stamps: Detailed stamp metadata (scraped from individual pages + enrichment)
CREATE TABLE IF NOT EXISTS stamps (
    slug TEXT PRIMARY KEY,
    api_slug TEXT NOT NULL,
    name TEXT NOT NULL,
    url TEXT NOT NULL,
    year INTEGER NOT NULL,
    issue_date TEXT,
    issue_location TEXT,
    rate TEXT,
    rate_type TEXT,
    type TEXT NOT NULL DEFAULT 'stamp',
    series TEXT,
    stamp_images TEXT,  -- JSON array
    sheet_image TEXT,
    credits TEXT,       -- JSON object
    about TEXT,
    background_color TEXT,
    forever INTEGER NOT NULL DEFAULT 0,
    value INTEGER,              -- enriched
    value_type TEXT,            -- enriched
    full_bleed INTEGER,         -- enriched
    shape TEXT,                 -- enriched
    words TEXT                  -- enriched (JSON array)
);

CREATE INDEX IF NOT EXISTS idx_stamps_year ON stamps(year);
CREATE INDEX IF NOT EXISTS idx_stamps_api_slug ON stamps(api_slug);

-- products: Purchasable items associated with stamps
CREATE TABLE IF NOT EXISTS products (
    stamp_slug TEXT NOT NULL,
    year INTEGER NOT NULL,
    title TEXT NOT NULL,
    long_title TEXT,
    price TEXT,
    postal_store_url TEXT,
    stamps_forever_url TEXT,
    images TEXT,  -- JSON array
    metadata TEXT,  -- JSON object with parsed product attributes
    PRIMARY KEY (stamp_slug, title)
);

CREATE INDEX IF NOT EXISTS idx_products_stamp_slug ON products(stamp_slug);
