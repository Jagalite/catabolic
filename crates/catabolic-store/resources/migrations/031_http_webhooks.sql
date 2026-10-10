CREATE TABLE api_webhooks(
 profile TEXT NOT NULL, destination_id TEXT NOT NULL,
 principal TEXT NOT NULL REFERENCES api_principals(id),
 token_id TEXT NOT NULL REFERENCES api_tokens(id),
 url TEXT NOT NULL, request_id TEXT UNIQUE REFERENCES api_requests(id),
 PRIMARY KEY(profile,destination_id),
 FOREIGN KEY(profile,destination_id) REFERENCES notification_destinations(profile,id)
);
CREATE INDEX api_webhooks_request ON api_webhooks(request_id);
CREATE TRIGGER webhook_request_events AFTER UPDATE OF state ON api_requests
WHEN NEW.state != OLD.state AND NEW.state IN ('ready','failed','cancelled','stale','blocked') BEGIN
 INSERT INTO consumer_events(id,profile,event,severity,subject)
 SELECT 'request:' || lower(hex(randomblob(16))), NEW.profile,
 'request_' || NEW.state, CASE NEW.state WHEN 'failed' THEN 'error' ELSE 'info' END, NEW.id
 WHERE EXISTS(SELECT 1 FROM api_webhooks WHERE request_id=NEW.id);
 INSERT INTO notification_deliveries(event_id,profile,destination_id)
 SELECT e.id,e.profile,d.id FROM consumer_events e
 JOIN api_webhooks w ON w.request_id=NEW.id AND w.profile=e.profile
 JOIN notification_destinations d ON d.profile=w.profile AND d.id=w.destination_id
 WHERE e.rowid=last_insert_rowid() AND e.subject=NEW.id AND e.event='request_' || NEW.state AND d.enabled=1;
END;
