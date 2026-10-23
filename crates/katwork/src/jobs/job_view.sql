SELECT j.id,to_jsonb(j.status) AS status,to_jsonb(j.reason) AS reason,
       to_jsonb(CASE WHEN j.status IS NULL THEN t.hold_reason END) AS hold_reason,
       to_jsonb(CASE WHEN j.status IS NULL AND t.state='queued' THEN 1+(
           SELECT count(*) FROM tasks ahead JOIN jobs running ON running.id=ahead.job_id AND running.workspace_id=ahead.workspace_id
           WHERE ahead.workspace_id=$1 AND ahead.kat_id=t.kat_id AND ahead.state='queued'
             AND running.status IS NULL AND (ahead.created_at,ahead.id)<(t.created_at,t.id)
       ) END) AS queue_position
FROM jobs j LEFT JOIN LATERAL (
    SELECT state,hold_reason,kat_id,created_at,id FROM tasks
    WHERE workspace_id=$1 AND job_id=j.id ORDER BY created_at DESC,id DESC LIMIT 1
) t ON true
WHERE j.workspace_id=$1 AND j.id=$2
