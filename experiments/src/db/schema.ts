/**
 * IronAir booking database: 7 tables.
 *
 * Answering a single "can I cancel my ticket and what do I get back?" question
 * requires joining ALL of these:
 *
 *   passengers ──< bookings ──< booking_segments >── flights
 *                      │               │
 *                   payments        tickets ──< refund_requests
 *
 * That 7-table join is the "one-page query" this experiment is about.
 */
export const SCHEMA_DDL = `
CREATE TABLE passengers (
  id            INTEGER PRIMARY KEY,
  first_name    TEXT NOT NULL,
  last_name     TEXT NOT NULL,
  email         TEXT NOT NULL UNIQUE,
  loyalty_tier  TEXT NOT NULL DEFAULT 'none',   -- none | silver | gold | platinum
  created_at    TEXT NOT NULL
);

CREATE TABLE bookings (
  id            INTEGER PRIMARY KEY,
  reference     TEXT NOT NULL UNIQUE,           -- e.g. 'X7K2PQ'
  passenger_id  INTEGER NOT NULL REFERENCES passengers(id),
  status        TEXT NOT NULL,                  -- confirmed | completed | cancelled
  channel       TEXT NOT NULL,                  -- web | mobile | agent
  created_at    TEXT NOT NULL
);

CREATE TABLE flights (
  id            INTEGER PRIMARY KEY,
  flight_number TEXT NOT NULL,                  -- e.g. 'IC204'
  origin        TEXT NOT NULL,
  destination   TEXT NOT NULL,
  departure_at  TEXT NOT NULL,                  -- ISO 8601
  arrival_at    TEXT NOT NULL,
  status        TEXT NOT NULL                   -- scheduled | delayed | cancelled
);

CREATE TABLE booking_segments (
  id            INTEGER PRIMARY KEY,
  booking_id    INTEGER NOT NULL REFERENCES bookings(id),
  flight_id     INTEGER NOT NULL REFERENCES flights(id),
  cabin_class   TEXT NOT NULL                   -- economy | premium | business
);

CREATE TABLE tickets (
  id                 INTEGER PRIMARY KEY,
  booking_segment_id INTEGER NOT NULL REFERENCES booking_segments(id),
  fare_class         TEXT NOT NULL,             -- Y | B | M | K | J
  fare_basis         TEXT NOT NULL,             -- e.g. 'YFLEX', 'MECO'
  price_cents        INTEGER NOT NULL,
  refundable         INTEGER NOT NULL,          -- 0 | 1
  status             TEXT NOT NULL              -- active | cancelled | refunded
);

CREATE TABLE payments (
  id            INTEGER PRIMARY KEY,
  booking_id    INTEGER NOT NULL REFERENCES bookings(id),
  method        TEXT NOT NULL,                  -- card | miles | voucher
  amount_cents  INTEGER NOT NULL,
  status        TEXT NOT NULL,                  -- captured | refunded | pending
  paid_at       TEXT NOT NULL
);

CREATE TABLE refund_requests (
  id            INTEGER PRIMARY KEY,
  ticket_id     INTEGER NOT NULL REFERENCES tickets(id),
  reason        TEXT,
  status        TEXT NOT NULL,                  -- pending | approved | denied
  requested_at  TEXT NOT NULL,
  processed_at  TEXT
);

CREATE INDEX idx_bookings_passenger ON bookings(passenger_id);
CREATE INDEX idx_segments_booking   ON booking_segments(booking_id);
CREATE INDEX idx_tickets_segment    ON tickets(booking_segment_id);
CREATE INDEX idx_payments_booking   ON payments(booking_id);
CREATE INDEX idx_refunds_ticket     ON refund_requests(ticket_id);
`;
