import { Database } from './database.js';
import { SCHEMA_DDL } from './schema.js';

export const DEFAULT_SEED = 42;

export const FIRST_NAMES = [
  'Ava', 'Liam', 'Maya', 'Noah', 'Priya', 'Lucas', 'Sofia', 'Ethan', 'Ingrid', 'Mateo',
  'Zoe', 'Felix', 'Nina', 'Oscar', 'Lena', 'Ravi', 'Emma', 'Jonas', 'Mia', 'Kian',
];

export const LAST_NAMES = [
  'Andersen', 'Beaumont', 'Castillo', 'Dubois', 'Eriksen', 'Fischer', 'Garcia', 'Haugen',
  'Ivanov', 'Johansson', 'Kowalski', 'Larsen', 'Moreau', 'Novak', 'Ortiz', 'Petrov',
  'Quinn', 'Rossi', 'Silva', 'Tanaka',
];

const AIRPORTS = ['OSL', 'ARN', 'CPH', 'HEL', 'BER', 'AMS', 'LHR', 'CDG', 'JFK', 'SFO'];
const LOYALTY_TIERS = ['none', 'none', 'none', 'silver', 'silver', 'gold', 'platinum'];
const CHANNELS = ['web', 'web', 'mobile', 'agent'];
const CABIN_CLASSES = ['economy', 'economy', 'economy', 'premium', 'business'];
const FARE_CLASSES = ['Y', 'B', 'M', 'K', 'J'] as const;
const PAYMENT_METHODS = ['card', 'card', 'card', 'card', 'miles', 'voucher'];
const REFUND_REASONS = ['plans changed', 'meeting moved', 'illness', 'duplicate booking', null];

const PASSENGER_COUNT = 150;
const FLIGHT_COUNT = 40;
const BOOKING_COUNT = 220;

/** A customer whose booking is used as the subject of one passenger request in the experiment. */
export interface ScenarioCustomer {
  firstName: string;
  lastName: string;
  reference: string;
  hasRefundRequest: boolean;
}

/** Deterministic PRNG so every run (and every scenario) sees the exact same data. */
export function mulberry32(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a |= 0;
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

export function pick<T>(rng: () => number, items: readonly T[]): T {
  const item = items[Math.floor(rng() * items.length)];
  if (item === undefined) throw new Error('pick() called with an empty array');
  return item;
}

export function randInt(rng: () => number, min: number, max: number): number {
  return min + Math.floor(rng() * (max - min + 1));
}

function iso(date: Date): string {
  return date.toISOString();
}

function daysFromNow(days: number, hour: number): Date {
  const date = new Date();
  date.setDate(date.getDate() + days);
  date.setUTCHours(hour, 0, 0, 0);
  return date;
}

function bookingReference(rng: () => number, used: Set<string>): string {
  const alphabet = 'ABCDEFGHJKLMNPQRSTUVWXYZ23456789';
  let ref = '';
  do {
    ref = Array.from({ length: 6 }, () => alphabet[Math.floor(rng() * alphabet.length)]).join('');
  } while (used.has(ref));
  used.add(ref);
  return ref;
}

/** Builds a fresh in-memory database with identical content for a given seed. */
export function createSeededDatabase(seed: number = DEFAULT_SEED): Database {
  const rng = mulberry32(seed);
  const db = new Database();
  db.exec(SCHEMA_DDL);

  seedFlights(db, rng);
  seedPassengers(db, rng);
  seedBookings(db, rng);
  return db;
}

function seedFlights(db: Database, rng: () => number): void {
  for (let id = 1; id <= FLIGHT_COUNT; id++) {
    const origin = pick(rng, AIRPORTS);
    let destination = pick(rng, AIRPORTS);
    while (destination === origin) destination = pick(rng, AIRPORTS);

    const departure = daysFromNow(randInt(rng, 1, 45), randInt(rng, 5, 22));
    const arrival = new Date(departure.getTime() + randInt(rng, 1, 11) * 3_600_000);
    const status = rng() < 0.1 ? 'delayed' : 'scheduled';

    db.run(
      'INSERT INTO flights (id, flight_number, origin, destination, departure_at, arrival_at, status) VALUES (:id, :fn, :o, :d, :dep, :arr, :st)',
      { id, fn: `IC${100 + id}`, o: origin, d: destination, dep: iso(departure), arr: iso(arrival), st: status },
    );
  }
}

function seedPassengers(db: Database, rng: () => number): void {
  const usedEmails = new Set<string>();
  for (let id = 1; id <= PASSENGER_COUNT; id++) {
    const firstName = pick(rng, FIRST_NAMES);
    let lastName = pick(rng, LAST_NAMES);
    let email = `${firstName.toLowerCase()}.${lastName.toLowerCase()}@example.com`;
    while (usedEmails.has(email)) {
      lastName = pick(rng, LAST_NAMES);
      email = `${firstName.toLowerCase()}.${lastName.toLowerCase()}@example.com`;
    }
    usedEmails.add(email);

    db.run(
      'INSERT INTO passengers (id, first_name, last_name, email, loyalty_tier, created_at) VALUES (:id, :fn, :ln, :em, :lt, :ca)',
      {
        id, fn: firstName, ln: lastName, em: email,
        lt: pick(rng, LOYALTY_TIERS), ca: iso(daysFromNow(-randInt(rng, 30, 700), 12)),
      },
    );
  }
}

function seedBookings(db: Database, rng: () => number): void {
  const usedReferences = new Set<string>();
  let segmentId = 1;
  let ticketId = 1;

  for (let id = 1; id <= BOOKING_COUNT; id++) {
    const statusRoll = rng();
    const status = statusRoll < 0.7 ? 'confirmed' : statusRoll < 0.85 ? 'completed' : 'cancelled';
    const createdAt = iso(daysFromNow(-randInt(rng, 1, 30), randInt(rng, 8, 20)));

    db.run(
      'INSERT INTO bookings (id, reference, passenger_id, status, channel, created_at) VALUES (:id, :ref, :pid, :st, :ch, :ca)',
      {
        id, ref: bookingReference(rng, usedReferences), pid: randInt(rng, 1, PASSENGER_COUNT),
        st: status, ch: pick(rng, CHANNELS), ca: createdAt,
      },
    );

    // 80% direct flight, 20% one connection.
    const flightIds = [randInt(rng, 1, FLIGHT_COUNT)];
    if (rng() < 0.2) flightIds.push(randInt(rng, 1, FLIGHT_COUNT));

    let totalPriceCents = 0;
    for (const flightId of flightIds) {
      const cabin = pick(rng, CABIN_CLASSES);
      db.run(
        'INSERT INTO booking_segments (id, booking_id, flight_id, cabin_class) VALUES (:id, :bid, :fid, :cc)',
        { id: segmentId, bid: id, fid: flightId, cc: cabin },
      );

      const fareClass = pick(rng, FARE_CLASSES);
      const refundable = fareClass === 'Y' || fareClass === 'J' || rng() < 0.1;
      const priceCents = randInt(rng, 80, 900) * 100;
      totalPriceCents += priceCents;
      const ticketStatus = status === 'cancelled' ? 'cancelled' : 'active';

      db.run(
        'INSERT INTO tickets (id, booking_segment_id, fare_class, fare_basis, price_cents, refundable, status) VALUES (:id, :sid, :fc, :fb, :pc, :rf, :st)',
        {
          id: ticketId, sid: segmentId, fc: fareClass,
          fb: `${fareClass}${refundable ? 'FLEX' : 'ECO'}`, pc: priceCents,
          rf: refundable ? 1 : 0, st: ticketStatus,
        },
      );

      // ~12% of tickets on confirmed bookings already have a refund request in flight.
      if (status === 'confirmed' && rng() < 0.12) {
        const rrStatus = pick(rng, ['pending', 'approved', 'denied'] as const);
        const requestedAt = iso(daysFromNow(-randInt(rng, 0, 5), randInt(rng, 8, 20)));
        db.run(
          'INSERT INTO refund_requests (ticket_id, reason, status, requested_at, processed_at) VALUES (:tid, :rsn, :st, :ra, :pa)',
          {
            tid: ticketId, rsn: pick(rng, REFUND_REASONS), st: rrStatus, ra: requestedAt,
            pa: rrStatus === 'pending' ? null : requestedAt,
          },
        );
      }

      segmentId += 1;
      ticketId += 1;
    }

    db.run(
      'INSERT INTO payments (booking_id, method, amount_cents, status, paid_at) VALUES (:bid, :m, :a, :st, :pa)',
      {
        bid: id, m: pick(rng, PAYMENT_METHODS), a: totalPriceCents,
        st: status === 'cancelled' ? 'refunded' : 'captured', pa: createdAt,
      },
    );
  }
}

/**
 * Picks `count` customers with confirmed bookings on future flights, each a distinct
 * passenger. Where possible, the customer at index 4 has an existing refund request
 * (one of the scripted requests asks about it).
 */
export function pickScenarioCustomers(db: Database, count = 6): ScenarioCustomer[] {
  const rows = db.query(
    `SELECT DISTINCT p.first_name AS firstName, p.last_name AS lastName, b.reference AS reference,
            EXISTS (
              SELECT 1 FROM booking_segments bs
              JOIN tickets t ON t.booking_segment_id = bs.id
              JOIN refund_requests rr ON rr.ticket_id = t.id
              WHERE bs.booking_id = b.id
            ) AS hasRefundRequest
     FROM passengers p
     JOIN bookings b ON b.passenger_id = p.id
     JOIN booking_segments bs ON bs.booking_id = b.id
     JOIN flights f ON f.id = bs.flight_id
     WHERE b.status = 'confirmed' AND f.departure_at > datetime('now')
     ORDER BY b.id`,
  ).rows as unknown as {
    firstName: string;
    lastName: string;
    reference: string;
    hasRefundRequest: number;
  }[];

  const seenPassengers = new Set<string>();
  const candidates = rows.filter((row) => {
    const key = `${row.firstName} ${row.lastName}`;
    if (seenPassengers.has(key)) return false;
    seenPassengers.add(key);
    return true;
  });
  if (candidates.length < count) {
    throw new Error(`Seed produced only ${candidates.length} eligible customers, need ${count}.`);
  }

  // Evenly spaced deterministic sample across the candidate list.
  const step = Math.floor(candidates.length / count);
  const selected = Array.from({ length: count }, (_, i) => {
    const candidate = candidates[Math.min(i * step, candidates.length - 1)];
    if (!candidate) throw new Error('Scenario customer selection failed.');
    return candidate;
  });

  const customers: ScenarioCustomer[] = selected.map((c) => ({
    firstName: c.firstName,
    lastName: c.lastName,
    reference: c.reference,
    hasRefundRequest: c.hasRefundRequest === 1,
  }));

  // Try to ensure slot 4 (the "has a refund already been requested?" request) has one:
  // first swap within the selection, otherwise pull any eligible candidate into slot 4.
  const refundIndex = 4;
  if (!customers[refundIndex]?.hasRefundRequest) {
    const donorInSelection = customers.findIndex((c) => c.hasRefundRequest);
    const current = customers[refundIndex];
    if (donorInSelection >= 0 && current) {
      const donor = customers[donorInSelection];
      if (donor) {
        customers[refundIndex] = donor;
        customers[donorInSelection] = current;
      }
    } else if (current) {
      const donor = candidates.find(
        (c) =>
          c.hasRefundRequest === 1 &&
          !customers.some((s) => s.reference === c.reference),
      );
      if (donor) {
        customers[refundIndex] = {
          firstName: donor.firstName,
          lastName: donor.lastName,
          reference: donor.reference,
          hasRefundRequest: true,
        };
      }
    }
  }
  return customers;
}
