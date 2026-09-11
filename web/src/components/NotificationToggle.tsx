import { Switch } from '@/components/ui/switch'

type NotificationToggleProps = {
  checked: boolean
  disabled?: boolean
  onCheckedChange: (checked: boolean) => void
  label?: string
  description?: string
}

export default function NotificationToggle({
  checked,
  disabled,
  onCheckedChange,
  label = 'Notifications',
  description = 'Get a push notification when the session needs attention or exits.',
}: NotificationToggleProps) {
  return (
    <div className="flex items-center justify-between gap-3 rounded-md border border-[hsl(var(--border))] bg-[hsl(var(--muted))]/40 px-3 py-2.5">
      <div className="flex flex-col gap-0.5">
        <span className="text-xs font-medium text-[hsl(var(--foreground))]">{label}</span>
        <span className="text-[11px] text-[hsl(var(--muted-foreground))]">{description}</span>
      </div>
      <Switch
        checked={checked}
        disabled={disabled}
        onCheckedChange={onCheckedChange}
        aria-label={label}
        className="shrink-0"
      />
    </div>
  )
}
