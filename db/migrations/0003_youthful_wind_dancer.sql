CREATE TABLE `automations` (
	`id` text PRIMARY KEY NOT NULL,
	`data` text NOT NULL
);
--> statement-breakpoint
ALTER TABLE `sessions` ADD `originating_automation` text;