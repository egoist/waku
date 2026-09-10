import {
  DarkTheme,
  DefaultTheme,
  router,
  Stack,
  ThemeProvider,
  type NativeStackNavigationOptions,
} from "expo-router";
import * as SplashScreen from "expo-splash-screen";
import { StatusBar } from "expo-status-bar";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { useEffect, useMemo } from "react";
import { StyleSheet, useColorScheme } from "react-native";
import { GestureHandlerRootView } from "react-native-gesture-handler";

import { Colors } from "@/constants/theme";
import {
  HeaderAction,
  HeaderActionGroup,
  nativeHeaderButtons,
  type HeaderActionSpec,
} from "@/components/screen-header";
import { TaskDrawerHost, useTaskDrawer } from "@/components/task-drawer";
import { DaemonProvider, useDaemon } from "@/lib/daemon-context";
import { RuntimeProvider } from "@/lib/runtime-context";
import { KeyboardOffsetProvider } from "@/lib/keyboard-offset";

/** Deep links and state restores keep the new-task home as the stack anchor. */
export const unstable_settings = { anchor: "index" };

void SplashScreen.preventAutoHideAsync();
SplashScreen.setOptions({ duration: 250, fade: true });

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      retry: 1,
      refetchOnReconnect: true,
    },
  },
});

/**
 * Native navigation bar with no background of its own: content scrolls
 * beneath it and its Liquid Glass items float. Every screen on the main path
 * shows the bar, the task list included, because that is what lets UIKit hold
 * the bar's buttons in place and crossfade them during an interactive
 * swipe-back; popping to a screen without a bar makes UIKit slide the whole
 * bar out with the page instead.
 */
const floatingHeader = {
  headerShown: true,
  headerStyle: { backgroundColor: "transparent" },
  headerTransparent: true,
} satisfies NativeStackNavigationOptions;

export default function RootLayout() {
  const colorScheme = useColorScheme();
  const colors = Colors[colorScheme === "dark" ? "dark" : "light"];
  const navigationTheme =
    colorScheme === "dark"
      ? {
          ...DarkTheme,
          colors: {
            ...DarkTheme.colors,
            background: colors.background,
            card: colors.background,
          },
        }
      : {
          ...DefaultTheme,
          colors: {
            ...DefaultTheme.colors,
            background: colors.background,
            card: colors.background,
          },
        };
  return (
    <GestureHandlerRootView style={styles.root}>
      <KeyboardOffsetProvider>
        <QueryClientProvider client={queryClient}>
        <DaemonProvider>
          <RuntimeProvider>
            <ThemeProvider value={navigationTheme}>
              <TaskDrawerHost>
                <AppNavigator />
              </TaskDrawerHost>
              <StatusBar style="auto" />
            </ThemeProvider>
          </RuntimeProvider>
        </DaemonProvider>
      </QueryClientProvider>
      </KeyboardOffsetProvider>
    </GestureHandlerRootView>
  );
}

function AppNavigator() {
  const { phase, profiles, booted } = useDaemon();
  const { openTaskDrawer } = useTaskDrawer();
  const colorScheme = useColorScheme();
  const theme = Colors[colorScheme === "dark" ? "dark" : "light"];
  const daemonRoutesAvailable = phase === "booting" || profiles.length > 0;
  const drawerAction = useMemo<HeaderActionSpec>(() => ({
    icon: { ios: "sidebar.left", android: "menu", web: "menu" },
    label: "Task history",
    onPress: openTaskDrawer,
  }), [openTaskDrawer]);
  const drawerHeader = useMemo<NativeStackNavigationOptions>(() => ({
    ...floatingHeader,
    gestureEnabled: false,
    headerLeft: () => (
      <HeaderActionGroup>
        <HeaderAction {...drawerAction} />
      </HeaderActionGroup>
    ),
    unstable_headerLeftItems: () => nativeHeaderButtons([drawerAction]),
  }), [drawerAction]);
  /** Usage is a screen you visit deliberately, so it hangs off the home and
   * new-task bars rather than the drawer, which belongs to task history. */
  const usageAction = useMemo<HeaderActionSpec>(() => ({
    icon: { ios: "chart.bar", android: "bar_chart", web: "bar_chart" },
    label: "Usage",
    onPress: () => router.push("/usage"),
  }), []);
  const settingsAction = useMemo<HeaderActionSpec>(() => ({
    icon: { ios: "gearshape", android: "settings", web: "settings" },
    label: "Settings",
    onPress: () => router.push("/settings"),
  }), []);
  const homeHeader = useMemo<NativeStackNavigationOptions>(() => ({
    ...drawerHeader,
    headerRight: () => (
      <HeaderActionGroup>
        <HeaderAction {...usageAction} />
        <HeaderAction {...settingsAction} />
      </HeaderActionGroup>
    ),
    unstable_headerRightItems: () => nativeHeaderButtons([usageAction, settingsAction]),
  }), [drawerHeader, settingsAction, usageAction]);

  useEffect(() => {
    // Hide once bootstrap has settled (or the safety timeout fired) — not on the
    // connection phase, which can stall and wedge the splash after a force-kill.
    if (booted) void SplashScreen.hideAsync();
  }, [booted]);

  return (
    <Stack
      screenOptions={{
        contentStyle: { backgroundColor: theme.background },
        headerBackButtonDisplayMode: "minimal",
        headerShadowVisible: false,
        headerStyle: { backgroundColor: theme.background },
        headerTintColor: theme.text,
      }}
    >
      <Stack.Screen
        name="index"
        options={daemonRoutesAvailable
          ? { ...homeHeader, title: "New Task" }
          : { headerShown: false, title: "Waku" }}
      />
      {/* Removing the final saved daemon also removes every daemon-backed
       * route from navigation, returning restored and open tasks to home. */}
      <Stack.Protected guard={daemonRoutesAvailable}>
        <Stack.Screen
          name="daemons"
          options={{ headerLargeTitle: true, title: "Daemons" }}
        />
        <Stack.Screen
          name="new-task"
          options={{ ...homeHeader, title: "New Task" }}
        />
        <Stack.Screen
          name="usage"
          options={{ title: "Usage" }}
        />
        <Stack.Screen
          name="settings"
          options={{ title: "Settings" }}
        />
        <Stack.Screen
          name="skills"
          options={{ title: "Skills" }}
        />
        <Stack.Screen
          name="session/[id]"
          options={{
            ...drawerHeader,
            // Chat header must be a solid surface, not the floating glass
            // treatment — content behind it is a live transcript, not a scroll
            // view meant to bleed under the bar.
            headerTransparent: false,
            headerStyle: { backgroundColor: theme.background },
            animation: "none",
            headerTitleAlign: "left",
            title: "Task",
          }}
        />
      </Stack.Protected>
      <Stack.Screen
        name="daemon-editor"
        options={{
          // Full-screen push, not a pageSheet modal: a modal steals the first
          // tap after presentation (focus can't land on the address/token
          // fields until a dismiss gesture is registered), so the form feels
          // like its inputs need multiple taps.
          title: "Add Daemon",
        }}
      />
    </Stack>
  );
}

const styles = StyleSheet.create({
  root: { flex: 1 },
});
