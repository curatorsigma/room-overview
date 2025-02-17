-- UP booking table
CREATE TABLE bookings (
	booking_id INTEGER PRIMARY KEY,
	title TEXT NOT NULL,
	resource_id INTEGER NOT NULL,
	start_time DATETIME NOT NULL,
	end_time DATETIME NOT NULL
);

