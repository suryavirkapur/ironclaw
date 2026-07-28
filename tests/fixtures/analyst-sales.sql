CREATE TABLE sales (
    sold_on date NOT NULL,
    region text NOT NULL,
    product text NOT NULL,
    units integer NOT NULL CHECK (units > 0),
    revenue numeric(12, 2) NOT NULL CHECK (revenue >= 0)
);

INSERT INTO sales (sold_on, region, product, units, revenue) VALUES
    ('2026-07-01', 'Dubai', 'Sandbox Pro', 12, 18000.00),
    ('2026-07-02', 'Delhi', 'Sandbox Pro', 9, 13500.00),
    ('2026-07-03', 'London', 'Sandbox Pro', 5, 7500.00),
    ('2026-07-08', 'Dubai', 'Agent Core', 18, 21600.00),
    ('2026-07-09', 'Delhi', 'Agent Core', 21, 25200.00),
    ('2026-07-10', 'London', 'Agent Core', 7, 8400.00),
    ('2026-07-15', 'Dubai', 'Data Studio', 11, 24200.00),
    ('2026-07-16', 'Delhi', 'Data Studio', 14, 30800.00),
    ('2026-07-17', 'London', 'Data Studio', 6, 13200.00);

CREATE ROLE analyst LOGIN PASSWORD 'analyst-demo';
GRANT CONNECT ON DATABASE ironclaw_analytics TO analyst;
GRANT USAGE ON SCHEMA public TO analyst;
GRANT SELECT ON ALL TABLES IN SCHEMA public TO analyst;
ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT SELECT ON TABLES TO analyst;
