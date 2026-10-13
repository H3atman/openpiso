<script lang="ts">
  import { passwordChangeErrorMessage } from "$lib/api";
  import type { PasswordChangeError } from "$lib/types";
  import ChangePasswordDialog from "$lib/components/ChangePasswordDialog.svelte";

  interface HarnessController {
    readonly calls: number;
    resolve: () => void;
    reject: (error: PasswordChangeError) => void;
  }

  let busy = $state(false);
  let calls = $state(0);
  let resolvePending: (() => void) | null = null;
  let rejectPending: ((error: Error) => void) | null = null;

  function submit(): Promise<void> {
    calls += 1;
    busy = true;
    return new Promise((resolve, reject) => {
      resolvePending = resolve;
      rejectPending = reject;
    });
  }

  const controller: HarnessController = {
    get calls() {
      return calls;
    },
    resolve() {
      busy = false;
      resolvePending?.();
      resolvePending = null;
      rejectPending = null;
    },
    reject(error) {
      busy = false;
      rejectPending?.(new Error(passwordChangeErrorMessage(error)));
      resolvePending = null;
      rejectPending = null;
    },
  };

  const harnessWindow = window as typeof window & {
    passwordChangeHarness?: HarnessController;
  };
  harnessWindow.passwordChangeHarness = controller;
</script>

<ChangePasswordDialog open={true} {busy} onSubmit={submit} onClose={() => {}} />
