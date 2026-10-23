SELECT GREATEST(0, (EXTRACT(EPOCH FROM LEAST(
    t.first_claimed_at + $1 * INTERVAL '1 microsecond',
    j.created_at + $2 * INTERVAL '1 microsecond'
) - clock_timestamp()) * 1000000)::bigint) AS remaining_micros
FROM tasks t JOIN jobs j ON j.id=t.job_id AND j.workspace_id=t.workspace_id
WHERE t.workspace_id=$3 AND t.id=$4
