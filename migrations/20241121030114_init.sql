-- Add migration script here
CREATE TABLE received_mentions (
    from_url TEXT NOT NULL,
    to_path TEXT NOT NULL,
    PRIMARY KEY (from_url, to_path)
);