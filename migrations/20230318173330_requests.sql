CREATE TABLE requests (
	id varchar(255) NOT NULL PRIMARY KEY,
	hash varchar(66) NOT NULL,
	tx JSON NOT NULL,
	mined boolean NOT NULL DEFAULT false,
	chain int unsigned NOT NULL 
);