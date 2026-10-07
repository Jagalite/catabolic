CREATE TRIGGER notification_job_events AFTER UPDATE OF state ON processing_jobs
WHEN NEW.state != OLD.state AND NEW.state IN ('complete','failed','timeout') BEGIN
 INSERT INTO consumer_events(id,profile,event,severity,subject)
 VALUES('job:' || lower(hex(randomblob(16))), NEW.profile,
 CASE NEW.state WHEN 'complete' THEN 'job_completed' ELSE 'job_failed' END,
 CASE NEW.state WHEN 'complete' THEN 'info' ELSE 'error' END, NEW.id);
 INSERT INTO notification_deliveries(event_id,profile,destination_id)
 SELECT e.id, e.profile, d.id FROM consumer_events e
 JOIN notification_destinations d ON d.profile=e.profile
 WHERE e.rowid=last_insert_rowid() AND d.enabled=1
 AND EXISTS(SELECT 1 FROM json_each(d.subscriptions) WHERE value=e.event)
 AND (e.severity='error' OR d.min_severity='info');
END;
