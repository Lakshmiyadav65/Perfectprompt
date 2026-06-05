import { createClient, type SupabaseClient } from "@supabase/supabase-js";

const supabaseUrl = import.meta.env.VITE_SUPABASE_URL as string | undefined;
const supabaseAnonKey = import.meta.env.VITE_SUPABASE_ANON_KEY as string | undefined;

export const isSupabaseConfigured = Boolean(supabaseUrl && supabaseAnonKey);

if (!isSupabaseConfigured) {
  console.warn(
    "[supabase] VITE_SUPABASE_URL or VITE_SUPABASE_ANON_KEY missing — hosted tier disabled, BYOK path only",
  );
}

export const supabase: SupabaseClient = createClient(
  supabaseUrl ?? "https://placeholder.invalid",
  supabaseAnonKey ?? "placeholder",
  {
    auth: {
      flowType: "pkce",
      detectSessionInUrl: false,
      persistSession: true,
      autoRefreshToken: true,
      storage: typeof window !== "undefined" ? window.localStorage : undefined,
    },
  },
);

/// OAuth flows redirect here from Supabase. We route through a
/// public web page on the GitHub Pages landing site instead of
/// straight at the `perfectprompt://` deep link so the user lands
/// on a branded "Signed in" confirmation page and triggers the
/// browser's "Open PerfectPrompt?" launch dialog only by explicit
/// click — feels intentional rather than an abrupt OS prompt.
///
/// The page itself lives at web/auth-success.html in this repo and
/// forwards all query params (code, type=recovery, error) to
/// `perfectprompt://auth/callback` when the user clicks Open.
///
/// MUST be added to Supabase's "Redirect URLs" allowlist in the
/// Auth → URL Configuration dashboard, otherwise OAuth will refuse
/// to redirect here and fall back to a 403.
export const REDIRECT_URL = "https://perfectprompt-beta.vercel.app/auth-success.html";
