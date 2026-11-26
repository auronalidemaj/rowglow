PRAGMA foreign_keys = ON;
CREATE TABLE artists (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE, country TEXT);
CREATE TABLE albums (
  id INTEGER PRIMARY KEY,
  artist_id INTEGER NOT NULL REFERENCES artists(id) ON DELETE CASCADE,
  title TEXT NOT NULL,
  year INTEGER CHECK (year > 1900),
  rating REAL,
  cover BLOB
);
CREATE INDEX albums_artist ON albums(artist_id);
CREATE TABLE tracks (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  album_id INTEGER NOT NULL REFERENCES albums(id),
  title TEXT NOT NULL,
  seconds INTEGER NOT NULL,
  lyrics TEXT
);
INSERT INTO artists (name, country) VALUES ('Nils Frahm','DE'),('Ólafur Arnalds','IS'),('Hania Rani','PL'),('Max Richter','DE'),('Joep Beving','NL');
INSERT INTO albums (artist_id, title, year, rating, cover) VALUES
 (1,'Spaces',2013,4.6,x'89504E47'),(1,'All Melody',2018,4.4,NULL),(2,'re:member',2018,4.5,NULL),
 (3,'Esja',2019,4.7,NULL),(4,'The Blue Notebooks',2004,4.9,NULL),(5,'Solipsism',2015,4.2,NULL);
WITH RECURSIVE seq(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM seq WHERE n < 300)
INSERT INTO tracks (album_id, title, seconds, lyrics)
SELECT 1 + (n % 6), 'Track ' || n, 120 + (n * 37) % 400, CASE WHEN n % 9 = 0 THEN 'la la la' || char(10) || 'second line' ELSE NULL END FROM seq;
CREATE VIEW album_lengths AS SELECT a.title, count(t.id) AS tracks, sum(t.seconds) AS seconds FROM albums a JOIN tracks t ON t.album_id = a.id GROUP BY a.id;
CREATE TRIGGER albums_touch AFTER UPDATE ON albums BEGIN UPDATE albums SET rating = CASE WHEN NEW.rating > 5 THEN 5 ELSE NEW.rating END WHERE id = NEW.id AND NEW.rating > 5; END;
