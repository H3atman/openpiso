<script lang="ts">
  import { Input } from "$lib/components/ui/input";

  /**
   * The DD:HH:mm:ss entry for a Voucher's time value, as four labelled number
   * fields under one "Time" legend. Range checks stay with the caller, which
   * reports them on submit.
   */
  let {
    idPrefix,
    days = $bindable(),
    hours = $bindable(),
    minutes = $bindable(),
    seconds = $bindable(),
    class: className = "",
  }: {
    /** Prefix for the four field ids, e.g. "voucher-time" → "voucher-time-days". */
    idPrefix: string;
    days: number;
    hours: number;
    minutes: number;
    seconds: number;
    class?: string;
  } = $props();
</script>

<fieldset class="min-w-0 {className}">
  <legend class="mb-1.5 text-sm font-medium">Time</legend>
  <div class="grid grid-cols-4 gap-2">
    <div class="min-w-0">
      <label class="mb-1 block text-xs text-muted-foreground" for="{idPrefix}-days">Days</label>
      <Input id="{idPrefix}-days" type="number" min="0" max="99" inputmode="numeric" class="tabular-nums" bind:value={days} />
    </div>
    <div class="min-w-0">
      <label class="mb-1 block text-xs text-muted-foreground" for="{idPrefix}-hours">Hours</label>
      <Input id="{idPrefix}-hours" type="number" min="0" max="23" inputmode="numeric" class="tabular-nums" bind:value={hours} />
    </div>
    <div class="min-w-0">
      <label class="mb-1 block text-xs text-muted-foreground" for="{idPrefix}-minutes">Minutes</label>
      <Input id="{idPrefix}-minutes" type="number" min="0" max="59" inputmode="numeric" class="tabular-nums" bind:value={minutes} />
    </div>
    <div class="min-w-0">
      <label class="mb-1 block text-xs text-muted-foreground" for="{idPrefix}-seconds">Seconds</label>
      <Input id="{idPrefix}-seconds" type="number" min="0" max="59" inputmode="numeric" class="tabular-nums" bind:value={seconds} />
    </div>
  </div>
</fieldset>
